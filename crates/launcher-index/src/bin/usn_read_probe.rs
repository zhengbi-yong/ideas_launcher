//! 试一遍读变更日志的几种方式,看内核肯理哪一种。
//!
//! 要以管理员身份跑:读日志需要「管理卷」的特权。

use launcher_index::usn;

fn main() {
    println!("== 读变更日志(FSCTL_READ_USN_JOURNAL) ==");
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
        for line in usn::probe_read_usn_journal(&volume) {
            println!("  {line}");
        }
    }
}
