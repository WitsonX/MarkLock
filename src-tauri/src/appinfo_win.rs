//! Windows 下解析快速启动项（.exe / .lnk 等）的显示名与图标。
//!
//! 走隐藏控制台的 PowerShell + System.Drawing 提取，避免为此引入重型 Win32/PNG 依赖：
//! - 显示名：`.lnk` 取快捷方式文件名（去扩展名，即用户看到的名字）；`.exe` 优先取版本信息
//!   FileDescription / ProductName（本地化应用名），回退文件名。中文系统即得中文名。
//! - 图标：`Icon.ExtractAssociatedIcon` 抽文件关联图标 → PNG → base64 data URL。
//!
//! 与 macOS 的 `appinfo` 提供同名 `display_name` / `icon_data_url`，供 lib.rs 命令按平台分派。
#![cfg(target_os = "windows")]

use base64::Engine;

/// PowerShell 单引号字符串内以两个单引号转义，避免路径含引号时语法错误 / 注入。
fn ps_quote(path: &str) -> String {
    path.replace('\'', "''")
}

/// 运行一段 PowerShell 脚本（无配置文件、非交互、隐藏控制台窗口），成功时返回 stdout 文本。
fn run_powershell(script: &str) -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        // CREATE_NO_WINDOW：powershell 是控制台子系统程序，从 GUI 进程 spawn 默认会闪出控制台窗口。
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn display_name(path: &str) -> Option<String> {
    let p = ps_quote(path);
    let script = format!(
        "try{{[Console]::OutputEncoding=[System.Text.Encoding]::UTF8}}catch{{}}; \
         $fi=Get-Item -LiteralPath '{p}' -ErrorAction SilentlyContinue; \
         if($fi){{ \
           if($fi.Extension -eq '.lnk'){{ [Console]::Out.Write($fi.BaseName) }} \
           else {{ \
             $d=$fi.VersionInfo.FileDescription; \
             if([string]::IsNullOrWhiteSpace($d)){{$d=$fi.VersionInfo.ProductName}}; \
             if([string]::IsNullOrWhiteSpace($d)){{$d=$fi.BaseName}}; \
             [Console]::Out.Write($d) \
           }} \
         }} else {{ [Console]::Out.Write((Split-Path -Leaf '{p}')) }}",
        p = p
    );
    let s = run_powershell(&script)?;
    let name = s.trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

pub fn icon_data_url(path: &str) -> Option<String> {
    let p = ps_quote(path);
    let script = format!(
        "Add-Type -AssemblyName System.Drawing; \
         $ic=[System.Drawing.Icon]::ExtractAssociatedIcon('{p}'); \
         if($ic){{ \
           $ms=New-Object System.IO.MemoryStream; \
           $bm=$ic.ToBitmap(); \
           $bm.Save($ms,[System.Drawing.Imaging.ImageFormat]::Png); \
           [Console]::Out.Write([Convert]::ToBase64String($ms.ToArray())); \
           $bm.Dispose(); $ms.Dispose(); $ic.Dispose() \
         }}",
        p = p
    );
    let s = run_powershell(&script)?;
    // 控制台可能对超长行折行插入换行/空格——剔除所有空白即得纯净 base64。
    let b64: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if b64.is_empty() {
        return None;
    }
    if base64::engine::general_purpose::STANDARD.decode(&b64).is_err() {
        return None;
    }
    Some(format!("data:image/png;base64,{b64}"))
}
