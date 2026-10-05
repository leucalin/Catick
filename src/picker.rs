//! 系统文件选择框：优先 zenity（GTK），退而求其次 kdialog（KDE）。
//!
//! 对话框会阻塞等待用户操作，因此调用方必须放在后台线程里执行，
//! 选中的路径再通过命令通道送回主循环。

use std::path::PathBuf;

/// 弹出文件选择框；用户取消或系统没有可用的对话框程序时返回 `None`。
pub fn pick_file(
    title: &str,
    filter_name: &str,
    patterns: &[&str],
    start_dir: &str,
) -> Option<PathBuf> {
    let filter = format!("{filter_name} | {}", patterns.join(" "));

    let mut zenity = std::process::Command::new("zenity");
    zenity
        .arg("--file-selection")
        .arg(format!("--title={title}"))
        .arg(format!("--file-filter={filter}"));
    if !start_dir.is_empty() {
        zenity.arg(format!("--filename={start_dir}/"));
    }
    if let Ok(out) = zenity.output() {
        return parse_path(out);
    }

    let mut kdialog = std::process::Command::new("kdialog");
    kdialog
        .arg("--getopenfilename")
        .arg(if start_dir.is_empty() { "." } else { start_dir })
        .arg(patterns.join(" "));
    if let Ok(out) = kdialog.output() {
        return parse_path(out);
    }

    eprintln!("catick: 未找到 zenity / kdialog，无法弹出文件选择框");
    None
}

fn parse_path(out: std::process::Output) -> Option<PathBuf> {
    if !out.status.success() {
        return None; // 用户取消
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}
