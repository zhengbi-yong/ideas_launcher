//! 看主文件表枚举出来的记录里带不带变更号。
//!
//! 用法(需要管理员):`mft_delta.exe`

use std::time::Instant;

use launcher_index::usn;

fn main() {
    println!("== 主文件表的记录带不带变更号 ==");
    let volumes = usn::ntfs_volumes();
    for volume in &volumes {
        let Some(state) = usn::journal_state(volume) else {
            println!("{}: 问不到日志状态", volume.root);
            continue;
        };
        let head = state.next_usn;
        println!(
            "{}: 日志 first=0x{:x} next=0x{:x}",
            volume.root, state.first_usn, head
        );
        let started = Instant::now();
        let mut count = 0usize;
        let mut shown = 0usize;
        let mut min_usn = u64::MAX;
        let mut max_usn = 0u64;
        let mut above = 0usize;
        let since = head - 60_000_000;
        let result = usn::enumerate_changed_since(volume, 0, head, |_name, entry| {
            count += 1;
            let usn = entry.usn;
            if usn < min_usn {
                min_usn = usn;
            }
            if usn > max_usn {
                max_usn = usn;
            }
            if usn as i64 > since {
                above += 1;
            }
            if shown < 5 {
                shown += 1;
                println!(
                    "  第 {count} 条: usn=0x{usn:x} reason=0x{:x} 名字={:?}",
                    entry.reason,
                    _name
                );
            }
        });
        let elapsed = started.elapsed();
        match result {
            Ok(_) => println!(
                "  整张表: {count} 条，用时 {elapsed:?}；变更号 0x{min_usn:x}..0x{max_usn:x}；其中比 {since} 新的 {above} 条",
            ),
            Err(error) => println!("  失败 {error:?}，用时 {elapsed:?}"),
        }
    }
}
