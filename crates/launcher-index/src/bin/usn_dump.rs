//! 把变更日志的原始字节存到文件里,好一条一条看。
//!
//! 要以管理员身份跑:读日志需要「管理卷」的特权。
//!
//! 用法:usn_dump <要写的文件> [从哪个 USN 开始]

use launcher_index::usn;
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(target) = args.next() else {
        eprintln!("用法: usn_dump <要写的文件> [从哪个 USN 开始]");
        std::process::exit(2);
    };
    let start: i64 = args
        .next()
        .and_then(|text| text.parse().ok())
        .unwrap_or(0);
    let filter = args.next().unwrap_or_default();

    println!("== 存下变更日志的原始字节 ==");
    let privilege = usn::enable_manage_volume_privilege_detailed();
    println!(
        "「管理卷」的特权: {}",
        if privilege.enabled {
            "拿到了 ✓"
        } else {
            "没拿到 ✗"
        }
    );

    for volume in usn::ntfs_volumes() {
        println!("-- {} --", volume.root);
        match usn::read_journal_raw(&volume, start, 300 * 1024 * 1024) {
            Ok((raw, returned)) => {
                println!("  读回 {returned} 字节，里面 {} 条记录", usn::count_raw_records(&raw));
                for line in usn::describe_raw_records(&raw, 4) {
                    println!("    {line}");
                }
                if !filter.is_empty() {
                    println!("  名字里有「{filter}」的记录:");
                    for line in usn::describe_records_matching(&raw, &filter, 20) {
                        println!("    {line}");
                    }
                }
                let name = format!("{}.bin", volume.root.trim_end_matches(['\\', ':']));
                let path = Path::new(&target).join(name);
                if filter.is_empty() {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match std::fs::write(&path, &raw) {
                        Ok(()) => println!("  原始字节写在 {}", path.display()),
                        Err(err) => println!("  写文件失败: {err}"),
                    }
                }
            }
            Err(err) => println!("  读不了: {err}"),
        }
    }
}
