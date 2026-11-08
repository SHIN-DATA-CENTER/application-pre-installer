# Read only the registered WinGet community source. Never open installed.db or write its index.
$source = Get-AppxPackage -Name Microsoft.Winget.Source -ErrorAction Stop | Sort-Object Version -Descending | Select-Object -First 1
if ($null -eq $source -or [string]::IsNullOrWhiteSpace($source.InstallLocation)) {
    throw 'WinGetの索引が見つかりません。WinGet一覧を取得・更新してから再試行してください。'
}
$index = Join-Path $source.InstallLocation 'Public/index.db'
if (!(Test-Path -LiteralPath $index -PathType Leaf)) { throw 'WinGetの索引ファイルがありません。WinGet一覧を再取得してください。' }
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
[assembly:DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
public static class WingetTagIndexReader {
    [DllImport("winsqlite3.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern int sqlite3_open_v2(byte[] path, out IntPtr db, int flags, IntPtr vfs);
    [DllImport("winsqlite3.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern int sqlite3_close(IntPtr db);
    [DllImport("winsqlite3.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern int sqlite3_prepare_v2(IntPtr db, byte[] sql, int length, out IntPtr stmt, IntPtr tail);
    [DllImport("winsqlite3.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern int sqlite3_step(IntPtr stmt);
    [DllImport("winsqlite3.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern int sqlite3_finalize(IntPtr stmt);
    [DllImport("winsqlite3.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern IntPtr sqlite3_column_text(IntPtr stmt, int column);
    [DllImport("winsqlite3.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern int sqlite3_column_bytes(IntPtr stmt, int column);
    static byte[] Utf8(string text) { return Encoding.UTF8.GetBytes(text + "\0"); }
    static List<string> ReadColumn(IntPtr db, string sql) {
        IntPtr stmt = IntPtr.Zero;
        try {
            int code = sqlite3_prepare_v2(db, Utf8(sql), -1, out stmt, IntPtr.Zero);
            if (code != 0 || stmt == IntPtr.Zero) throw new InvalidOperationException("索引の形式を認識できません (SQLite " + code + ")");
            var values = new List<string>();
            while ((code = sqlite3_step(stmt)) == 100) {
                IntPtr text = sqlite3_column_text(stmt, 0);
                int count = sqlite3_column_bytes(stmt, 0);
                if (text == IntPtr.Zero || count < 0 || count > 16384) throw new InvalidOperationException("索引の文字列が不正です");
                var bytes = new byte[count];
                Marshal.Copy(text, bytes, 0, count);
                values.Add(new UTF8Encoding(false, true).GetString(bytes));
                if (values.Count > 100000) throw new InvalidOperationException("索引のタグ数が上限を超えています");
            }
            if (code != 101) throw new InvalidOperationException("索引を読み取れません (SQLite " + code + ")");
            return values;
        } finally { if (stmt != IntPtr.Zero) sqlite3_finalize(stmt); }
    }
    public static string[] Read(string path) {
        IntPtr db = IntPtr.Zero;
        try {
            // SQLITE_OPEN_READONLY. A missing database must not be created.
            int code = sqlite3_open_v2(Utf8(path), out db, 1, IntPtr.Zero);
            if (code != 0 || db == IntPtr.Zero) throw new InvalidOperationException("WinGet索引を読み取り専用で開けません (SQLite " + code + ")");
            var tables = ReadColumn(db, "SELECT name FROM sqlite_master WHERE type='table' AND name IN ('tags2','tags')");
            if (tables.Contains("tags2")) return ReadColumn(db, "SELECT tag FROM tags2").ToArray();
            if (tables.Contains("tags")) return ReadColumn(db, "SELECT tag FROM tags").ToArray();
            throw new InvalidOperationException("このWinGet索引には対応するタグ表がありません");
        } finally { if (db != IntPtr.Zero) sqlite3_close(db); }
    }
}
'@
$tags = [WingetTagIndexReader]::Read($index)
@{ source_version=[string]$source.Version; tags=@($tags) } | ConvertTo-Json -Depth 3 -Compress
