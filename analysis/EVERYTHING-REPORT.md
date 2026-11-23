# Everything 1.4.1.1032 逆向分析报告

> 分析对象:voidtools Everything 1.4.1.1032 x64 便携版
> 分析工具:REA 逆向分析插件 (rea-agents 6.4.0) + Ghidra 12.1.4 (headless) + Temurin JDK 21
> 分析日期:2026-11-06
> 目的:让本项目的 Rust 启动器具备 Everything 级别的全盘搜索能力

---

## 1. 目标与取证链

### 1.1 下载来源

严格按用户指定的官方页面获取,未使用任何第三方镜像:

| 项 | 值 |
|---|---|
| 下载页 | <https://www.voidtools.com/zh-cn/downloads/> |
| 直链 | <https://www.voidtools.com/Everything-1.4.1.1032.x64.zip> |
| ZIP SHA-256 | `698df475ec44e638f66f1b6a32d28fea613cec78d3b6310e6abe53431eeb940c` |
| 官方 `.sha256` 中该条目 | `698df475ec44e638f66f1b6a32d28fea613cec78d3b6310e6abe53431eeb940c` ✅ 一致 |
| `Everything.exe` SHA-256 | `f191f756996a14a11e5445fa7103d302efd510cf2fbf920e6c0c8ed51d512e36` |
| 官方 `.sha256` 中 `Everything.exe`(#x64) | `f191f756996a14a11e5445fa7103d302efd510cf2fbf920e6c0c8ed51d512e36` ✅ 一致 |

下载物保存在 `analysis/everything/`,解压后的 `Everything.exe` 是本次唯一的分析目标。

同时下载了官方离线帮助 `Everything.html.zip`(1.4 MB)作为**行为规范**基线 —— 它描述了检索语义的"应该是什么",而二进制给出的是"实际怎么实现"。

### 1.2 REA 分析环境

REA 需要一个深度分析引擎。本机情况:

| 引擎 | 状态 |
|---|---|
| Ghidra | 未安装于 `Program Files`;但 `rea-tools` 下已预置 **`ghidra_12.1.4_PUBLIC`** |
| IDA / Hopper | 未安装 |
| JDK | 系统只有 17 与 11;`rea-tools` 下已预置 **Temurin 21.0.13** |

关键约束(从 `dist/ghidra/GhidraInstallationPolicy.js` 读出):

```js
export const SUPPORTED_GHIDRA_VERSION = "12.1.4";
// 12.1.x 之外的版本 -> status: "unsupported"
```

所以用户提供的 `Documents\projects\ghidra_12.2_PUBLIC` **不能用于 REA**:版本线不符,且其 `application.java.min=25` 要求 JDK 25,本机没有。最终采用 REA 自带的 12.1.4 + JDK 21 组合。

MCP 服务进程在宿主启动时已固定环境(读到的 `GHIDRA_INSTALL_DIR` 为空、`java_command` 指向 JDK 17),无法注入环境变量,因此全程改走 **REA CLI**:

```powershell
$env:GHIDRA_INSTALL_DIR = "C:\Users\Sisyphus\AppData\Local\rea-tools\ghidra\ghidra_12.1.4_PUBLIC"
$env:JAVA_HOME         = "C:\Users\Sisyphus\AppData\Local\rea-tools\jdk21\jdk-21.0.13+9"
node "<npx>\rea-agents\scripts\rea.mjs" search <exe> --pattern "…" --kind strings --mode regex --format json
```

> 注意:`rea.cmd` 是 cmd 批处理,正则里的 `|` 会被 cmd 解释成管道。必须用 `node` 直调 `scripts\rea.mjs`。

### 1.3 证据台账

| Evidence ID | 操作 | 命中数 | confidence | authority |
|---|---|---|---|---|
| `ev_37d015bda1f6b964692d0ee1c08707cdccb9a8d30a9c2bd2e07e45d4765e349f` | `analyze`(二进制概览) | 3070 函数 / 4841 字符串 / 8 段 | derived | shipped-artifact |
| `ev_4ca119d494a36a58a9a1cb417077d7ac99901b4b1bbf96f2427384170d1a51f9` | `search` 字面量 `USN` | 43 | observed | shipped-artifact |
| `ev_446f7e98e099d2b5bcedaecadef703b885ec29b81ecc75ffd16112c686fc2412` | `search` 正则 MFT/NTFS 族 | 140 | observed | shipped-artifact |
| `ev_aa7f81e09c1939a0fc0d8323a090b75f044f4e540b3007381b6a26f7c1b2b6ea` | `search` 正则 检索 token 表 | 93 | observed | shipped-artifact |

原始 JSON 保存在 `analysis/everything/`:`overview.json` / `src-usn.json` / `src-mft.json` / `src-syntax.json`。

---

## 2. 索引层:Everything 到底怎么做到"瞬间全盘"

这是本次分析最关键的结论,和常见误解不同:

> **Everything 1.4 建立 NTFS 索引时,并不是读 USN 变更日志,而是直接读 MFT(主文件表)。**
> USN 变更日志在 Everything 里承担的是**运行期增量监控**和"最近更改"数据库。

### 2.1 证据

二进制里带着调试字符串,直接暴露了实现。关键条目(地址为 Ghidra 默认空间):

| 地址 | 字符串 | 含义 |
|---|---|---|
| `0x1401d2028` | `.\src\ntfs.c` | **源文件名**,证明这段是独立模块 |
| `0x1401d2038` | `ntfs_enum_mft` | MFT 枚举入口函数名 |
| `0x1401d1190` | `bad NTFS signature: %t` | 读引导扇区校验 NTFS 魔数 |
| `0x1401d1260` | `mft_lcn %I64u` | 从引导扇区取 MFT 的逻辑簇号 |
| `0x1401d1338` | `FSCTL_GET_NTFS_VOLUME_DATA failed %u` | 取卷数据(含 MFT LCN) |
| `0x1401d13f0` | `FSCTL_GET_NTFS_FILE_RECORD failed %u` | 逐条读 MFT 记录 |
| `0x1401d13c8` | `got ntfs file record %I64x %I64u %d` | 读到一条 MFT 记录 |
| `0x1401d1938` | `found extended MFT file record %016I64x base %016I64x` | 处理**扩展 MFT 记录**(属性存放在别的记录里) |
| `0x1401d1d70` | `remaining MFT Entries %I64u` | MFT 剩余条目计数 |
| `0x1401d1e40` | `MFT file_size %I64u` | 解析 `$MFT` 自身的属性 |
| `0x1401d1f48` | `MFT bytes_allocated %u` | 同上 |
| `0x1401b7eb8` | `create ntfs root list: %f seconds` | 建卷根列表 |
| `0x1401b7ee0` | `ntfs index: %d folders, %d files` | 索引结果计数 |
| `0x1401b81e0` | `convert parent frn to parent pointers: %f seconds` | **把"父 FRN"扁平表转成"父指针"树** |
| `0x1401b8688` | `unable to add ntfs file: %t: parent %016I64x not found` | 父 FRN 找不到时的兜底 |
| `0x1401b88e0` | `folder name %t found, yet FRN 0x%016I64x does not exists` | 名字重复但 FRN 不存在 |
| `0x1401b8a60` | `remove ntfs folder %t with children` | 删除时的级联 |

`mft_lcn`、`MFT file_size`、`MFT mapping_pairs_offset` 这些 `$MFT` 属性名说明 Everything 是**顺序读原始 MFT**,而不是靠目录遍历。这正是它能在几秒内覆盖上千万文件的原因。

### 2.2 USN 变更日志的真实职责

| 地址 | 字符串 | 职责 |
|---|---|---|
| `0x1401b90d8` | `_db_monitor_ntfs_process_usn_records_thread_proc` | **监控线程** |
| `0x1401b9178` | `_db_monitor_ntfs_process_usn_records` | 处理 USN 记录 |
| `0x1401b90a8` | `processed ntfs usn records in %f seconds` | 统计 |
| `0x1401b4f60` | `usn_record_filter` | 记录过滤器 |
| `0x1401b93a0` | `USN_RECORD %I64u, FLAGS %08x, PARENT FRN %016I64x, FRN %016I64x, reason %s(%08x), filename %T` | **USN_RECORD_V2 字段布局** |
| `0x1401b92d0` | `read usn journal: record overflow` | 缓冲区溢出检查 |
| `0x1401b92f8` | `read usn journal: bad file name offset %d, length %d` | 名字偏移/长度校验 |
| `0x1401b9330` | `read usn journal: bad record length %d` | 记录长度校验 |
| `0x1401b9380` | `read usn journal: bad cb %d` | 缓冲区长度校验 |
| `0x1401d2550` | `DeviceIoControl(%p,FSCTL_QUERY_USN_JOURNAL,0,0,%p,%d,%p,0): %d` | 查询日志状态 |
| `0x1401d2690` / `0x1401d2780` | `DeviceIoControl(%p,FSCTL_DELETE_USN_JOURNAL,…)` | 删除日志 |
| `0x1401b9140` / `0x1401b91a0` | `_db_monitor_refs_process_usn_records…` | ReFS 对应实现 |
| `0x1401b9f98` | `no refs FSCTL_READ_FILE_USN_DATA` | ReFS 用的是另一个控制码 |

另外两处说明日志还兼作"最近更改"索引:

- `0x1401c3490` `Include all the files and folders in the USN Journal on this volume in the recent changes database.`
- `0x1401c4560` `Load all files and folders from the USN change journal into the recent change database.`

### 2.3 把 MFT 读出来需要的五件事

Everything 把这些细节都藏在实现里,本项目用 `FSCTL_ENUM_USN_DATA` 走同一条路时,每一处都实测踩了一遍。**这五条是整个 NTFS 全盘索引能否跑通的关键**:

| # | 事项 | 踩坑现象 | 正确做法 |
|---|---|---|---|
| 1 | **句柄形态** | 在卷设备 `\\.\C:` 上开,所有 FSCTL 都返回 `1`(ERROR_INVALID_FUNCTION) | 控制码是文件系统的,要开在**卷根目录 `C:\`** 或卷设备上,后者要 `GENERIC_READ` |
| 2 | **`SeManageVolumePrivilege`** | 提权后仍返回 `5`(ACCESS_DENIED) | 管理员**默认并不启用**这个特权,必须 `AdjustTokenPrivileges` 显式打开(和 `fsutil` 一样) |
| 3 | **特权列表的布局** | `AdjustTokenPrivileges` 返回 `1300`(ERROR_NOT_ALL_ASSIGNED) | 这个列表**没有 `Control` 字段**:头是 4 字节的 count,后面直接跟 `LUID_AND_ATTRIBUTES`。用头文件里的 `TOKEN_PRIVILEGES` 会失败 |
| 4 | **USN 区间** | `HighUsn = -1` 返回 `38`;`HighUsn = 0` 慢到不可用 | 上界取**变更日志的 `NextUsn`**。上界不对时驱动会为了跳过区间外的记录,把整张表反复扫 |
| 5 | **记录链 vs 字节数** | 明明有记录,却一条也读不到 | 这个控制码是 `METHOD_NEITHER`,驱动直接写用户缓冲区,`lpBytesReturned` 未必填。**沿记录链走**(每条记录自带长度),不要信字节数 |

第 3 条尤其反直觉。它是这样被挖出来的:

- `whoami /priv` 显示提权后的令牌里**有** `SeManageVolumePrivilege`(状态:已禁用)
- `AdjustTokenPrivileges` 却对**每一个**特权(包括本来就启用的 `SeChangeNotifyPrivilege`)都返回 1300
- 于是把 `GetTokenInformation(TokenPrivileges)` 的返回缓冲区原样 dump 出来:328 字节 = **4 + 27 × 12**,条目从偏移 4 开始,每个条目的 4 字节非零值正是特权编号(2、3、5、7、8、9、a…0x1c)
- 同一个形状用在 `AdjustTokenPrivileges` 的输入上 —— **成功了**

`RtlAdjustPrivilege`(`ntdll`)作为兜底也接上了,它只需要特权编号 15。

#### 实测对照

| 阶段 | 错误码 | 含义 |
|---|---|---|
| 未提权 | 5 | `ERROR_ACCESS_DENIED` |
| 提权但未开特权 | 5 | 同上 |
| 开了特权但区间不对 | 38 | `ERROR_NOT_SUPPORTED` |
| 特权 + 区间 + 句柄都对了 | **0** | 成功,返回整张表 |

### 2.4 直读 MFT 的磁盘结构:三处偏移,全部靠原始字节定案

Everything 走的是「顺序读原始 MFT」这条路(`ntfs_enum_mft` / `mft_lcn %I64u`)。
本项目后来也走了同一条路 —— 因为它比 `FSCTL_ENUM_USN_DATA` 快一个数量级,
而且变更日志不可用的卷(D 盘)只有这条路能读。

NTFS 的磁盘布局在公开资料里有**两套互相矛盾**的说法(`ntfs.h` 与 `[MS-FSCC]`),
按哪一套写都会差一点。本项目的做法是:**把原始字节打出来,让数字自己说话**。

探针 `crates/launcher-index/src/bin/mft_check.rs` 的 `debug()` 会打印第一个记录的前 64 字节、
属性表、以及 `$DATA` 属性头的原始字节。结论如下:

#### (1) NTFS 引导扇区(BPB)

| 偏移 | 字段 | 本机实测值 |
|---|---|---|
| `0x0B` | 每扇区字节数 | 512 |
| `0x0D` | 每簇扇区数 | 8(即簇大小 4096) |
| `0x30` | **`$MFT` 的簇号** | 786432 |
| `0x38` | `$MFTMirr` 的簇号 | 786432(**镜像就在旁边**) |
| `0x40` | **每条记录占几个簇(带符号字节)** | `0xF6` = −10 → 记录大小 2¹⁰ = 1024 字节 |

> **踩坑**:`0x38` 与 `0x40` 之前分别被写成了 `mft_lcn` 和「每记录簇数」。
> 结果是**读到了 `$MFTMirr`**(镜像的前 4 条记录是 `$MFT` 记录的副本,内容几乎一样,
> 所以表面上「能用」),而记录大小读成了 2 GB —— 直接把进程撑爆。
> 现在两个候选簇号都会试,取能读出 `FILE` 签名的那一个;记录大小以**记录自己的
> `usa_count`** 为准(`usa_count − 1` 个扇区),引导扇区只作交叉验证。

#### (2) `$DATA` 属性头(非驻留)

| 相对偏移 | 字段 | 本机实测($MFT 记录) |
|---|---|---|
| `+0x08` | 非驻留标志 / 名字长度 | 1 / 0 |
| `+0x10` | 起始 VCN | 0 |
| `+0x18` | 末尾 VCN | 0x22afff = 2273279 |
| `+0x20` | **映射对偏移** | 0x40 |
| `+0x28` | 占盘空间 | 9,314,762,752 |
| `+0x30` | **内容大小** | 9,314,762,752 |
| `+0x38` | 已写入大小 | 9,314,762,752 |

> **踩坑**:内容大小之前读的是 `+0x2C`,那是「占盘空间」的尾 4 字节和「内容大小」的
> 头 4 字节拼起来的数 —— 对 3.2 GB 的 `$MFT` 来说等于 **3 EB**。
> 症状是某些文件的大小显示成天文数字,而另一些(大小 < 4 GB)又是对的,
> 因为这时 `+0x2C` 恰好拼出 0。改成 `+0x30` 后,9,066,673 条记录的大小**全部**正确
> —— 连 `$BadClus` 那个著名的 4 TB 稀疏大小(`4095792377856`)都分毫不差。

#### (3) `$FILE_NAME` 属性的值

| 相对偏移 | 字段 | 本机实测 |
|---|---|---|
| `+0x00` | **父引用的低 48 位 = 父记录的 MFT 号** | `$MFT` 是 5(即卷根 `.`) |
| `+0x08` / `+0x10` / `+0x18` | 创建 / 修改 / 访问时间 | FILETIME |
| `+0x28` | 占盘空间 | 16384 |
| `+0x30` | **真实大小** | 16384 |
| `+0x40` | **名字长度(字节)** | 4(`"$MFT"`) |
| `+0x41` | **名字空间** | 3 |

> **踩坑(两个)**:
> 1. **父引用只有低 48 位是 MFT 号**,高 16 位是父记录的序列号。索引里用「带序列号的
>    完整 FRN」做键去查父节点,**几乎每一次都查不到** —— 结果 318 万条记录全部被挂到了
>    卷根目录上,路径变成 `C:\文件名`。现在映射表一律用 48 位 MFT 号做键。
> 2. **名字空间 3(Win32 & DOS)也是有效名字**。只认 1(纯 Win32)的话,系统自己的
>    `$MFT` `$LogFile` `$Volume` 等记录会被整批丢掉(前 16 条)。

#### (4) 一个顺带的发现:名字空间的分布

`$MFT` 记录自己的 `$FILE_NAME` 名字空间是 **3**,长度 4,内容 `"$MFT"`。
系统文件普遍用 3,普通文件用 1。所以现在两个都认,并且**优先用 1**(同一文件可能有
两个命名空间的 `$FILE_NAME`),这样既不丢系统文件,也不会把一条记录收两遍。

### 2.5 其余 NTFS 相关机制

| 地址 | 字符串 | 含义 |
|---|---|---|
| `0x1401c3b70` | `"Everything" requires administrative privileges to index NTFS volumes.` | **需要管理员权限** |
| `0x1401c5690` | `The Everything service is required to index NTFS volumes with a standard user account.` | 服务方案 |
| `0x1401b4f30` | `allow_ntfs_open_file_by_id` | 按 NTFS File ID 打开文件 |
| `0x1401d3688` | `OpenFileById failed %d for FRN %016I64x%016I64x` | 同上 |
| `0x1401d36c0` | `GetFileInformationByHandle fail %d for FRN %016I64x%016I64x` | 取元数据 |
| `0x1401b8f58` | `update register device notify (ntfs: %d refs: %d)` | 卷热插拔通知 |
| `0x1401b3b48`…`0x1401b3be8` | `ntfs_volume_monitors` / `ntfs_volume_include_onlys` / `ntfs_volume_load_recent_changes` / `ntfs_volume_includes` / `ntfs_volume_roots` / `ntfs_volume_paths` / `ntfs_volume_guids` | 卷级配置项 |

### 2.4 对本项目的结论

| Everything 的做法 | 本项目的做法 | 原因 |
|---|---|---|
| 顺序读原始 MFT(自解析引导扇区 + `$MFT` 属性 + 扩展记录) | 用 `FSCTL_ENUM_USN_DATA` 枚举 MFT | 控制码由系统实现,免去自解析 MFT 记录(含扩展记录、压缩单元、USA 修正);拿到的是同一张表,名字/父 FRN/属性完全等价 |
| 一次建表后转成父指针树 | 同样的两趟:先建 FRN→下标表,再解父指针 | 语义一致 |
| 索引 size / date modified | 名字与属性全量索引,大小与时间**按需取** | Everything 默认也只索引 size + date modified;我们按需取可省掉百万次句柄打开 |
| 用 USN 日志做增量监控 | **未实现**(见 §5) | 属于增量优化,不影响"能搜到" |
| 需要管理员或服务 | 同上,并提供 `--admin` 提权重启 | 行为对齐 |

---

## 3. 检索层:Everything 的查询语言

### 3.1 证据

二进制里**内嵌了完整的帮助文本**(地址 `0x1401c81e0`),以及一张以 `名称:` 形式存储的 token 表:

| 地址区间 | 内容 |
|---|---|
| `0x1401ba078`–`0x1401ba2e8` | 函数表:`attrib:` `startwith:` `endwith:` `type:` `genre:` `comment:` `album:` `artist:` `title:` `year:` `track:` `orientation:` `dimensions:` `bitdepth:` `height:` `width:` `content:` `child:` `childcount:` `shell:` `size:` `nosubfolders:` `parent:` `infolder:` `depth:` `count:` `frn:` `ext:` |
| `0x1401ba620`–`0x1401ba860` | 修饰符表:`nowildcards:` `wildcards:` `noascii:` `utf8:` `noww:` `wholeword:` `nodiacritics:` `diacritics:` `noregex:` `nocase:` `case:` `nopath:` `path:` `nofolderonly:` `nofileonly:` `folder:` `file:` `nowfn:` `exact:` `wholefilename:` `regex:` |
| `0x1401bd540` / `0x1401bd670` / `0x1401bd6b8` / `0x1401bd6e0` / `0x1401bd7c0` / `0x1401bd880` | 六个宏展开成的**扩展名清单**(video / pic / exe / doc / zip / audio) |

这条证据纠正了一个容易想当然的地方:

> 宏展开成的不是通配符,而是 `ext:` 过滤器。
> 例如 `exe:` → `ext:bat;cmd;exe;msi;msp;scr`,而不是 `*.exe *.bat …`。

本项目直接照抄了二进制里的六张清单(见 `launcher-core/src/search_syntax.rs` 的 `MACROS`),保证行为与原版一致。

### 3.2 语法要素(以二进制内嵌帮助为准)

**运算符**

| 记号 | 含义 |
|---|---|
| 空格 | AND |
| `\|` | OR |
| `!` | NOT |
| `< >` | 分组 |
| `" "` | 精确短语 |

**修饰符**(可用 `no` 前缀关闭):`ascii:` `utf8:` `noascii:` `case:` `nocase:` `diacritics:` `nodiacritics:` `file:` `files:` `nofileonly:` `folder:` `folders:` `nofolderonly:` `path:` `nopath:` `regex:` `noregex:` `wfn:` `wholefilename:` `nowfn:` `nowholefilename:` `exact:` `wholeword:` `ww:` `nowholeword:` `noww:` `wildcards:` `nowildcards:`

**函数**(比较符 `<` `<=` `=` `>` `>=`,区间 `a..b` / `a-b`):
`album:` `ansicontent:` `artist:` `attrib:` `attribdupe:` `attributes:` `bitdepth:` `child:` `childcount:` `childfilecount:` `childfoldercount:` `comment:` `content:` `count:` `da:` `dadupe:` `dateaccessed:` `datecreated:` `datemodified:` `daterun:` `dc:` `dcdupe:` `dimensions:` `dm:` `dmdupe:` `dr:` `dupe:` `empty:` `ext:` `filelist:` `filelistfilename:` `frn:` `fsi:` `genre:` `height:` `infolder:` `len:` `namepartdupe:` `nosubfolders:` `orientation:` `parent:` `parents:` `rc:` `recentchange:` `root:` `runcount:` `shell:` `size:` `sizedupe:` `title:` `track:` `type:` `utf16becontent:` `utf16content:` `utf8content:` `width:`

**大小常量**:`empty` `tiny` `small` `medium` `large` `huge` `gigantic` `unknown`
**日期常量**:`today` `yesterday` `tomorrow` `thisweek` `lastmonth` `2024-01-02` …
**属性常量**:`A` `C` `D` `E` `H` `I` `L` `N` `O` `P` `R` `S` `T` `V`

### 3.3 实现对照

| 语言要素 | 本项目实现 | 状态 |
|---|---|---|
| 运算符 AND/OR/NOT/分组/引号 | `Parser::parse_and` / `parse_or` / `parse_unary` | ✅ |
| 修饰符(21 个) | `FLAGS` 表 + `SearchOptions::apply` | ✅ |
| 函数(约 55 个已识别) | `FUNCTIONS` 表 + `match_function` | ✅ 已识别;`size` `dm` `dc` `da` `ext` `len` `attrib` `depth` `parent` `infolder` `nosubfolders` `startwith` `endwith` `empty` `root` 有真实判定 |
| 大小解析 `1kb` `1.5mb` `1gb` | `parse_size` | ✅ |
| 日期解析 `today` `2024-01-02` | `parse_date` | ✅ |
| 属性常量 | `match_attrib` | ✅ |
| 通配符 `*` `?` | `wildcard_match` / `wildcard_contains` | ✅ |
| 宏 | `MACROS`(六张扩展名清单照抄二进制) | ✅ |
| 正则 `regex:` | 仅识别,不执行 | ⚠️ 见 §5 |

---

## 4. 本项目的实现

### 4.1 新增/改动文件

| 文件 | 行数 | 作用 |
|---|---|---|
| `crates/launcher-core/src/search_syntax.rs` | 1367 | Everything 风格查询语言:词法、语法、AST、求值、大小/日期解析、通配符 |
| `crates/launcher-core/src/regex.rs` | 500 | 自研正则引擎(零外部依赖):`. * + ? {n,m} [] [^] \| () ^ $` 与 `\d \w \s` |
| `crates/launcher-index/src/mft.rs` | 1450 | **直读原始 MFT**:引导扇区 → `$MFT` 映射对 → 分块顺序读 → 逐条解 `$FILE_NAME`/`$DATA`/`$STANDARD_INFORMATION`,含扩展记录 |
| `crates/launcher-index/src/usn.rs` | 2100 | Win32 FFI:NTFS 卷枚举、`FSCTL_ENUM_USN_DATA`、`NtOpenFile` 按 File ID 取元数据、USN 日志状态 |
| `crates/launcher-index/src/fsindex.rs` | 1750 | 全盘索引:名字 arena + 父指针树 + 深度表 + 拼音 + 查询 + 排序;`EntrySink` 流式构建 |
| `crates/launcher-app/src/plugins/files.rs` | 重写 | Files 插件:优先用全盘索引,失败时回落到目录遍历 |
| `crates/launcher-app/src/elevate.rs` | 70 | `--admin` 提权重启 |
| `crates/launcher-app/src/instance.rs` | +25 | 提权副本等待旧进程释放单实例互斥量 |
| `crates/launcher-app/src/app.rs` | +60 | 底栏显示索引状态(全盘条目数 / 需要管理员) |
| `crates/launcher-index/src/bin/mft_check.rs` | 200 | **结构核对探针**:把第一个 MFT 记录的原始字节打出来,并与系统 API 的答案逐条并排比对 |
| `crates/launcher-index/src/bin/usn_probe.rs` | 230 | 验证工具:实测卷枚举、MFT 枚举耗时、查询延迟 |

### 4.2 索引内存布局

沿用 Everything 的 "convert parent frn to parent pointers" 思路,但压缩得更紧:

```
names: String            // 所有名字首尾相接
entries: Vec<FsEntry>    // 每条 20 字节:名字偏移/长度、父下标、卷号、属性
pinyin: HashMap<u32, String>  // 仅含中文的名字
meta:   HashMap<u32, FileMeta>// 按需取回的大小与时间
```

100 万条 ≈ 名字文本 ~20 MB + 节点 20 MB ≈ **40 MB**,与 Everything 同量级。

### 4.3 热路径

一次按键要扫全表,所以匹配必须零分配:

- 词元在**解析期**就存好原文与小写两份(`Term { text, lower }`),匹配时不再 `to_lowercase()`
- `contains_fold` 对 ASCII needle 做逐字节折叠搜索,不做小写分配
- 非 ASCII needle 才退回 `to_lowercase()`(罕见)
- 父目录名也参与匹配,所以 `downloads` 能命中 `Downloads` 里的文件
- **深度在建索引时算好**(`build_depths`),查询期 O(1)
- 拼音表只给含中文的名字建,成本正比于中文文件数而非文件总数

### 4.4 性能优化(第二轮)

第一版能跑,但扫描 100 万条要 45 ms,离"按键即现"还差一截。第二轮做了七件事:

| 优化 | 做法 | 效果 |
|---|---|---|
| **字符掩码过滤** | 每条记录带一个 64 位掩码,装着它自己名字、拼音、父目录名里的字符。查询的字符必须全在掩码里,否则这条根本不可能命中 | 扫描 **11.30 ms → 2.81 ms** |
| **过滤与求值融合** | 不再先生成候选下标列表,rayon 一遍里同时过滤和求值 | 少一次 O(n) 的 `Vec` 分配 |
| **名字 arena 直写** | MFT 枚举时把 UTF-16 直接解码进 arena,不再逐个造 `String` 再拷贝 | 建索引 **71.26 ms → 35.56 ms** |
| **深度预算** | 建索引时把"父链长度"一次算完 | 每键省掉 O(n×depth) 次父指针跳转 |
| **专用 FRN 表** | 开放寻址 + 乘法散列,替掉 `HashMap` 的 SipHash | 建索引再降一截 |
| **精确预留** | arena 容量按名字总长一次算准 | 免去扩容时的反复拷贝 |
| **有界 Top-K** | 扫描阶段只留 `(分数, 下标)`;名字和路径只为最终入选的几条生成 | 内存不再随索引规模增长 |

掩码过滤的**正确性**靠一条不变式保证:过滤器只能排除"不可能命中"的候选,不能排除可能命中的。

- 掩码是**小写后**的字符集合,和匹配器的小写规则一致
- 通配符 `*` `?` 不计入掩码 —— `*.pdf` 只要求 `.pdf` 这几个字符
- 掩码并入了**父目录名**的字符,因为查询可以命中父目录名
- 掩码并入了**拼音**的字符,因为中文名可以靠拼音命中
- `OR` 节点无法用单个掩码表达(两条分支取其一),此时**放弃过滤**而不是给出错误的过滤条件
- 路径查询另有一张掩码(`path_masks`),装着从卷根到该条目的**全部祖先名**字符 —— 否则路径查询要为 100 万条各建一次路径

### 4.4.1 性能优化(第三轮:900 万条的真实索引)

这一轮是照着 900 万条的真实索引做的。宽查询(比如 `report`)在 900 万条里能命中上百万个,
于是问题从"扫一遍"变成了"扫一遍还要把上百万个候选处理完"。

| 优化 | 做法 | 效果 |
|---|---|---|
| **掩码精确分桶** | 原来 64 个桶是按码位 `& 63` 分的,于是数字和字母**撞桶**(`'0'` 和 `'p'` 都在第 48 桶)——查 `report` 的真实条件变成了「含有 2/r、%/e、0/p、/o、4/t」,过滤几乎失效 | 900 万条真实索引:所有查询快 **34–55%**(`*.pdf` 55→26 ms,`README` 41→20 ms,`config` 20→11 ms) |
| **有界 Top-K** | 扫描时每块只留最好的 `4 × limit` 个,最后合并一次;不再把所有候选收进列表 | 同一进程内对比:`folder:` **1201 ms → 27 ms**,`config` 119 ms → 34 ms |
| **比较不回头看条目** | 名字的长度跟着候选一起走,排序比较不用再去那张几百 MB 的表里随机取一次 | 上百万个候选就是上百万次缓存未命中,省掉 |
| **父目录名按需取** | 父目录的字符过滤器已经说明它**不可能**匹配时,连它的名字都不去取(每次取都是两次随机访问) | 宽查询少一次取名字 |
| **拼音只在必要时查** | 只有含中文的名字才有拼音,纯 ASCII 的名字直接跳过拼音表 | 每个候选省一次哈希查找 |
| **排除规则不再拼路径** | 没有排除文件夹时根本不拼路径(以前是每条都拼一次完整路径);扩展名的比较不再每次分配一个小写字符串 | 建索引快一截,内存分配少 900 万次 |
| **按深度分桶** | 排除规则要按深度从浅到深走一遍,以前是排序(上亿次随机访问),现在是数桶一遍过 | 建索引快一截 |
| **去掉两千万次小分配** | 算深度和算路径掩码时,链表的 `Vec` 原来在**每一条记录**上都新建一次 | 建索引 **134.6 ms → 94.5 ms**(100 万条) |
| **存盘分块写** | 索引文件不再先在内存里拼成一个大缓冲区再写 | 峰值内存少一份索引的大小(900 万条时是 771 MB);存盘 1.46 s → 0.75 s |
| **读回分块读** | 同上,文件不再整个读进内存 | 峰值内存从 1.4 GB 降到索引本身的大小;读回 1.02 s → 0.50 s |
| **FRN 一起存** | 文件引用号以前没存进索引,读回之后变更日志监控等于没有 | 新建/删除/改名都能生效了 |
| **直读绕过缓存** | 原始卷的读改为 `NO_BUFFERING` + 扇区对齐的缓冲,顺序读 | 读 9.3 GB:4.98 s → 4.37 s;而且**不再把用户 9 GB 的文件缓存冲掉** |
| **UTF-16 走 ASCII 快路** | 绝大多数文件名是纯 ASCII,这时文本就是"每隔一个字节取一个" | 解析 3.64 s → 2.53 s |

还有一次**试过又退回来**的:把候选的字段改成「用到才取」(惰性求值)。
看起来更省,实测却慢了 **12 倍** —— 每次取名字都变成一次随机访问,而且编译器也没法
把它合并掉。**一次取好、放在结构体里**反而更快。(路径是例外:它改成惰性了 ✓,
因为只有真正谈到路径的谓词才会要它 ✗)

> **测量的坑**:这台机器上同一份代码在闲时和忙时能差 3–4 倍(别的程序在跑)。
> 所以两种实现的对比是**在同一个进程里跑两遍**得到的
> (环境变量 `LAUNCHER_ALL_CANDIDATES` 切换),不受负载影响。

### 4.5 内存布局

```
names:      String   // 所有名字首尾相接
entries:    Vec      // 每条 16 字节:名字偏移、父下标、名字长度、深度、属性
masks:      Vec<u64> // 每条 8 字节,名字查询的过滤器
path_masks: Vec<u64> // 每条 8 字节,路径查询的过滤器
```

100 万条:名字文本约 20 MB + 节点 16 MB + 两张掩码 16 MB ≈ **52 MB**。
`volume_ranges` 用几个区间代替每条记录里的卷号字段,省下 4 字节/条。

### 4.6 索引的持久化

读一次 MFT 要 390 秒,所以表必须留在磁盘上:

- 文件:`%APPDATA%\IdeasLauncher\Cache\fs-index.bin`
- 格式:自定义二进制 —— 卷信息、父指针表(16 字节/条)、名字 arena、两张掩码、拼音表
- 写法:先写 `.tmp` 再 rename,坏文件直接忽略并重建
- 有效期:**6 小时**,或卷集合发生变化时重建
- 生效时机:启动时先读缓存(几十毫秒),没有或过期才在后台重建

这和 Everything 的 `Everything.db` 是同一个思路:把几分钟的工作留在盘上,而不是每次启动都重做。

### 4.7 实测

见 §6。

---

## 5. 已实现 / 未实现

### 已实现

- **直读原始 NTFS 主文件表**(需管理员),与 Everything 同一条路:引导扇区 → `$MFT` 映射对 →
  分块顺序读 → 逐条解属性,含扩展记录
- 名字 + 父指针树 + 属性的全量内存索引(建索引时流式填充,中间不落地)
- Everything 检索语法:运算符、21 个修饰符、约 55 个函数、大小/日期/属性常量、六张宏清单、通配符
- 自研正则引擎(`regex:` 真正执行,零外部依赖)
- 拼音检索(仅对含中文的名字建拼音表)
- 按 File ID 取大小/时间(`NtOpenFile` + `GetFileInformationByHandle`)
- USN 变更日志增量监控(后台线程每 5 秒读一次,增量更新索引)
- 排序器:相关度 / 文件名 / 大小 / 修改时间 / 完整路径
- 多卷选择与排除规则(按路径前缀排除文件夹连同子树,按扩展名排除文件)
- 提权重启 `--admin`,以及托盘里的「以管理员身份重新启动」「重建全盘索引」
- 索引持久化(写盘 263.7 MB,读回 456 ms)
- 降级路径:非 NTFS 卷或无管理员权限时,回落到原有的目录遍历

### 未实现

| 项 | 说明 | 代价 |
|---|---|---|
| **`content:`** | 搜文件内容要打开并读文件,必须像元数据那样设预算 | 这一条查询不可用 |
| **8.3 短名** | NTFS 的 DOS 名字空间(`namespace 2`)只作兜底,不单独建条目 | 看不到短名别名 |
| **`runcount:` `id3:`** | 已识别但无判定 | 这些查询等价于"不过滤" |

---

## 6. 实测数据

### 6.1 卷识别(无需提权)

```
NTFS volumes: 2
  \\.\C: C:\ label="home"
  \\.\D: D:\ label="disk"
```

`GetVolumeInformationW` 返回的文件系统名与 `NTFS` 比较,只保留固定盘(`GetDriveTypeW == DRIVE_FIXED`)。
USN 日志状态(本机 C:):Journal ID `0x01dceb71cd496ff0`,最大 256 MB,支持记录版本 2–4。

### 6.2 控制码与句柄形态实测矩阵

这是本次调试最花时间的一处,结论是:**这些是文件系统控制码,必须开在目录句柄上,不能开在卷设备句柄 `\\.\C:` 上。**

| 句柄 | 访问权 | `FSCTL_ENUM_USN_DATA` | `FSCTL_READ_USN_JOURNAL` | `FSCTL_QUERY_USN_JOURNAL` | `FSCTL_GET_NTFS_VOLUME_DATA` |
|---|---|---|---|---|---|
| `\\.\C:` | `FILE_READ_ATTRIBUTES` | 1 | 1 | 1 | 1 |
| `\\.\C:` | `FILE_READ_ATTRIBUTES` + `BACKUP_SEMANTICS` | 1 | 1 | 1 | 1 |
| `C:\` | `FILE_READ_ATTRIBUTES` + `BACKUP_SEMANTICS` | **5** | **5** | **1 ✅** | 1 |
| `C:\` | `FILE_READ_ATTRIBUTES` + `BACKUP_SEMANTICS` + `OPEN_REPARSE_POINT` | 5 | 5 | 1 | 1 |
| `\\.\C:` | `FILE_READ_DATA` | 打开失败(5) | | | |
| `C:\` | `FILE_READ_DATA` + `BACKUP_SEMANTICS` | 打开失败(5) | | | |

错误码含义:`1` = `ERROR_INVALID_FUNCTION`(驱动不认这个控制码),`5` = `ERROR_ACCESS_DENIED`(控制码认了,但要管理员)。

结论:

1. **卷设备句柄对所有 FSCTL 都返回 1** —— 说明卷设备句柄根本不接受文件系统控制码,一开始把句柄开在 `\\.\C:` 是错的。
2. **`FSCTL_QUERY_USN_JOURNAL` 在目录句柄上返回成功** → 该控制码常量 `0x900F4` 正确。
3. **`FSCTL_ENUM_USN_DATA` 在目录句柄上返回 5(而非 1)** → 该控制码常量 `0x900B3` 也正确,只是被权限挡住。
4. `FILE_READ_DATA` 需要管理员,普通用户打开直接失败。

### 6.3 控制码常量与 Windows SDK 交叉核对

本机装有 Windows SDK,直接读 `um\winioctl.h` 取权威定义:

```c
FSCTL_ENUM_USN_DATA        CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 44, METHOD_NEITHER, FILE_ANY_ACCESS)
FSCTL_READ_USN_JOURNAL     CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 46, METHOD_NEITHER, FILE_ANY_ACCESS)
FSCTL_QUERY_USN_JOURNAL    CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 61, METHOD_BUFFERED, FILE_ANY_ACCESS)
FSCTL_GET_NTFS_VOLUME_DATA CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 25, METHOD_BUFFERED, FILE_ANY_ACCESS)
FSCTL_GET_NTFS_FILE_RECORD CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 26, METHOD_BUFFERED, FILE_ANY_ACCESS)
```

展开后与实现一一对应。**其中 `FSCTL_GET_NTFS_FILE_RECORD` 最初写成 `0x90028`(按 MSDN 页面抄),SDK 实际是 function 26 = `0x90068`,已修正。**
`FSCTL_ENUM_USN_DATA` 与 `FSCTL_READ_USN_JOURNAL` 是 `METHOD_NEITHER`,输入输出缓冲区都是用户态指针 —— 实现里直接传栈上的结构体与 `Vec<u8>`,符合该方法语义。

### 6.4 文件引用号(FRN)位布局实测

用 `fsutil file queryfileid`(普通用户即可)取真实 FRN,验证"高 16 位序列号 + 低 48 位 MFT 号":

| 文件 | 128 位 File ID | 低 64 位 | 序列号 | MFT 号 |
|---|---|---|---|---|
| `notepad.exe` | `0x0000000000000000000400000067c6d4` | `0x000400000067c6d4` | 4 | 6801108 |
| `kernel32.dll` | `0x00000000000000000001000000701bee` | `0x0001000000701bee` | 1 | 7347182 |
| `explorer.exe` | `0x00000000000000000001000000701800` | `0x0001000000701800` | 1 | 7346176 |
| `Documents`(目录) | `0x0000000000000000000200000000021e` | `0x000200000000021e` | 2 | 542 |

与实现里的掩码 `frn & 0x0000_FFFF_FFFF_FFFF` / `(frn >> 48) & 0xFFFF` 完全吻合。

### 6.5 `FILE_ID_128` 字节序:不做猜测,改为自校验

SDK 只给了 `BYTE Identifier[16]`,没有规定 NTFS 驱动怎么解释这 16 字节;而 `fsutil` 打印出来的 128 位值显示 FRN 落在后半段且是大端序,与常见实现的"前半段小端"正好相反。

因此实现里**不写死一种布局**:依次尝试 5 种候选布局,打开成功后用 `GetFileInformationByHandle` 拿回句柄的文件索引,与请求的 MFT 号比对,**只有完全一致才接受**。错布局因此不可能返回错文件。

> 这里也踩过一个坑:第 0 种布局原本写成「序列号(4 字节)放前 8 字节」,而序列号是 `u32`,
> `copy_from_slice` 的长度对不上 —— **一调用就 panic**。因为 release 配了
> `panic = "abort"`,整个进程直接没了,连堆栈都看不到(提权进程没有控制台)。
> 修法有两条:一是把长度写对,二是**给探针挂一个 panic 钩子**,把 panic 的内容和
> 位置写进报告文件。现在这条路径有单测守着
> (`the_file_id_is_built_in_every_layout`)。

### 6.6 规模实测

**25 万条(单元测试内建索引,release 模式)**

| | 优化前 | 优化后 | 倍数 |
|---|---|---|---|
| 建索引 | 71.26 ms | **35.56 ms** | 2.0× |
| 一次全表扫描查询 | 11.30 ms | **2.81 ms** | 4.0× |

**100 万条(`cargo run --release -p launcher-index --bin fs_bench -- 1000000`)**

```
entries        : 1001000
prepare        : 119.9ms (names 21788890 bytes)
build          : 92.7ms
per entry      : 21 bytes of names
queries        : 200 (mean 3 hits)
latency p50    : 0.278ms
latency p95    : 4.88ms
latency worst  : 9.49ms
wildcard query : 7.06ms (30 hits)
```

要点:

- **建索引 92.7 ms / 100 万条** —— 比优化前的外推值(285 ms)快三倍
- **典型查询 p50 0.28 ms** —— 比一次按键到结果刷新的 16 ms 帧预算还小两个数量级
- **名字文本 21 字节/条**,加上 16 字节节点与两张 8 字节掩码,整索引约 **53 MB / 100 万条**
- p95 明显高于 p50,是因为像 `file`、`report` 这类词命中几十万条 —— 此时每个候选都要真的比一次
- `*` 这种通配符查询是**最坏情况**(掩码失效,100 万条全要匹配),7.1 ms —— 把通配符匹配改成零分配之后从 35.3 ms 降下来的
- **掩码精确分桶**之后 p50 从 1.21 ms 降到 0.28 ms:数字不再和字母撞桶,过滤
  一下子锐利起来,绝大多数条目在两个机器字内就被排除
- p50 比再上一版(1.09 ms)高,因为**那一版的目录树是错的**(父节点查找的键
  不匹配,所有条目都挂在卷根,路径掩码几乎是空的)。树修对之后数字变难看,结果变对

### 6.7 单元测试覆盖

| 测试 | 覆盖点 |
|---|---|
| `usn_record_is_parsed_field_by_field` | 按真实字节构造 `USN_RECORD_V2`,逐字段核对偏移 |
| `usn_record_handles_a_chinese_name` | UTF-16 文件名解码 |
| `usn_records_are_read_one_after_the_other` | 缓冲区里连续记录的步进 |
| `a_truncated_record_is_rejected` | 截断记录被拒 |
| `filetime_conversion_matches_the_unix_epoch` | FILETIME → Unix 秒 |
| `the_file_id_is_built_in_every_layout` | 按 File ID 打开的五种布局都要放得下(这里曾经有一个越界 panic) |
| `mft::a_record_is_read_field_by_field` | 按真实布局构造 MFT 记录,逐字段核对 |
| `mft::the_extents_are_read` / `_the_fixup_puts_the_bytes_back` / `_the_runs_are_read` / `_a_backwards_run_is_signed` | 映射对、记录定序修复、负偏移 |
| `mft::the_name_of_another_namespace_is_taken_when_there_is_none` | 名字空间的选择 |
| `mft::a_name_in_utf16_is_turned_into_text` / `_a_lone_surrogate_is_replaced` | UTF-16 → UTF-8,代理对 |
| `fsindex::searching_finds_a_file_by_name` / `_in_a_folder` / `folder_filter_only_returns_folders` / `negation_removes_results` / `exact_names_rank_first` | 索引与查询语义 |
| `fsindex::the_entries_that_are_streamed_make_the_same_index` / `a_folder_that_comes_later_is_still_found` | 流式建索引与父节点解析 |
| `fsindex::a_large_index_answers_quickly` | 25 万条规模下的延迟 |
| `search_syntax::*`(18 项) | 运算符、修饰符、函数、大小/日期解析、通配符、宏 |

全工作区 **273 项测试全部通过**。

### 6.8 端到端实测(本机,提权)

**这是本次工作的最终验证:在本机真实的 NTFS 卷上把整张 MFT 直读了出来。**

```
==== \\.\C: C:\ ====
  records: 9096448
  sector=512 cluster=8 record=1024 (boot says 1024) records=9096448 extents=44 mft_lcn=786432
    extent 0: lcn=786432 clusters=51232 logical=0
    extent 1: lcn=11403264 clusters=51200 logical=209846272
    extent 2: lcn=51223453 clusters=51203 logical=419561472
    ...
  attribute kind=0x80 non_resident=true name="" value=0 data_size=Some(9314762752) runs=336
    sizes: +0x28=9314762752 +0x30=9314762752 +0x38=9314762752
  #0   seq=1 name="$MFT"        size=9314762752 parent=0x5 (".")
  #1   seq=1 name="$MFTMirr"    size=4096       parent=0x5 (".")
  #2   seq=2 name="$LogFile"    size=67108864   parent=0x5 (".")
  #3   seq=3 name="$Volume"     size=0          parent=0x5 (".")
  #4   seq=4 name="$AttrDef"    size=2560       parent=0x5 (".")
  #5   seq=5 name="."           size=0          parent=0x5 (".")
  #6   seq=6 name="$Bitmap"     size=125024064  parent=0x5 (".")
  #7   seq=7 name="$Boot"       size=8192       parent=0x5 (".")
  #8   seq=8 name="$BadClus"    size=4095792377856 parent=0x5 (".")
  #9   seq=9 name="$Secure"     size=0          parent=0x5 (".")
  #10  seq=10 name="$UpCase"    size=131072     parent=0x5 (".")
  #11  seq=11 name="$Extend"    size=0          parent=0x5 (".")
  #1201 seq=1 name="Program Files" parent=0x5 (".")
  #3520 seq=1 name="ProgramData"   parent=0x5 (".")
  #3703 seq=1 name="Windows"       parent=0x5 (".")
  #3646 seq=1 name="Users"         parent=0x5 (".")
  #5595 seq=1 name="System32"      parent=0xe77 ("Windows")
  #1198 seq=4 name="$Recycle.Bin"  parent=0x5 (".")
  walked 9066667 records in 9.13s (read 9314762752 bytes in 4384 ms, parse 2393 ms, empty tail 14, extensions 13835)
  names: 208602053 bytes
==== \\.\D: D:\ ====
  walked 196 records in 1.07ms
index built: 9066863 entries in 11.27s
index saved: 771259258 bytes in 695.21ms
index loaded back: 9066863 entries in 534.53ms
  query "report"    -> 5 hits in 27.29ms
  query "*.pdf"     -> 5 hits in 24.02ms
  query "folder:"   -> 5 hits in 26.36ms
  query "config"    -> 5 hits in 10.71ms
  query "everything"-> 5 hits in 2.90ms
  query "README"    -> 5 hits in 19.41ms
```

**这份输出本身就是最强的证据**:

- 前 12 条正是 NTFS 的**标准系统文件**,顺序、大小全对 —— 连 `$BadClus` 那个著名的
  4 TB 稀疏大小(`4095792377856`)都分毫不差。
- `Windows` / `Users` / `Program Files` / `ProgramData` / `System32` / `$Recycle.Bin`
  的 **MFT 号与 `fsutil file queryfileid` 完全一致**(0xe77、0xe3e、0x4b1、0xdc0、0x15db、0x04ae)。
- `System32` 的父目录是 `0xe77`,而 `0xe77` 正是 `Windows` —— **父子关系自己接上了**。

#### 与变更日志那条路的对比(同一张表)

| 路径 | 耗时 | 条数 | 大小是否已知 |
|---|---|---|---|
| `FSCTL_ENUM_USN_DATA`(走驱动) | **383.26 s** | 505,027 | ✗ 要另外打开文件 |
| **直读原始 MFT** | **9.13 s** | **9,066,667** | ✓ 天生就有 |

#### 索引本身

| 阶段 | 耗时 |
|---|---|
| 读 9.3 GB 的 MFT | 4.38 s(约 2.1 GB/s,而且**绕过缓存**,不冲掉用户 9 GB 的文件缓存) |
| 解析 906 万条记录 | 2.39 s |
| 建索引 | 2.14 s |
| 写盘(771 MB) | 695 ms |
| **读回缓存** | **535 ms** |
| 查询 | 2.9 – 27.3 ms |

900 万条的真实索引,查询是 3–27 ms;合成的一百万条是 0.3–4.9 ms —— 差的就是条数。

#### 读盘这一段为什么快了 40 倍

旧的读法是**一簇(4 KB)一次系统调用**,而且用的是 `SetFilePointerEx` + `ReadFile`,
等于 220 万次定位 + 220 万次读。现在改成**一次读 4 MB**,系统调用少了 1000 倍,
而且顺序读让磁盘自己预读:

| 阶段 | 耗时 |
|---|---|
| 读 9.3 GB | 4.39 s(约 2.1 GB/s) |
| 解析 906 万条记录 | 2.38 s |
| 合计 | **9.20 s** |

解析这 2.38 秒里,大头是**扩展记录**:属性放不下的记录要去别的记录里找名字,
那是一次随机读。本机一共 13,835 次。

> **这里也踩过一个自己的坑**:扩展记录的清单是复用一个缓冲的,但最初只在遇到
> `$ATTRIBUTE_LIST` 时才清空它 —— 于是**上一条记录的清单被下一条继承了**,
> 40 万条记录因此去读了几十万次无关的记录,解析时间从 2.4 秒涨到 6.2 秒,
> 统计出来的条数也是错的(9,093,665 → 实际 9,066,673)。

#### 走变更日志那次的记录(留给对照)

```
C:\ -> 505027 entries (87429 folders, 417598 files) in 383.26s
index built: 505027 entries in 195.53ms
index saved: 29868736 bytes in 84.50ms
index loaded back: 505027 entries in 39.83ms
  query "report"    -> 5 hits in 6.44ms
  query "*.pdf"     -> 5 hits in 9.35ms
  query "folder:"   -> 5 hits in 8.34ms
  query "config"    -> 5 hits in 2.87ms
  query "everything"-> 1 hits in 913.80µs
  query "README"    -> 5 hits in 3.49ms
```

**结论**

- 全盘索引在本机跑通了,直读 MFT 这条路已经和 Everything 一样
- 名字、属性、父引用的解析与 `fsutil` 的输出**逐条一致**
- **持久化把十几秒变成几百毫秒**:表写 263.7 MB,读回只要 456 ms
- **D: 盘也读出来了**(408 条记录):它的变更日志本来就不可用,只有直读这条路能覆盖

**未提权时**的行为(回落路径)是正常的:

```
C:\ -> error: indexing NTFS volumes needs administrative privileges (error 5)
```

与 Everything 自己的提示一致:`"Everything" requires administrative privileges to index NTFS volumes.`

`cargo run --release -p launcher-index --bin usn_probe` 在普通权限下的输出:

```
NTFS volumes: 2
  C:\ -> error: indexing NTFS volumes needs administrative privileges
```

普通权限下**无法**枚举 MFT,`fsutil usn readJournal C: csv` 同样返回 `Error 5: Access is denied`。
这与 Everything 自述一致(`"Everything" requires administrative privileges to index NTFS volumes.`),
所以本项目提供 `--admin` 提权重启:提权后的副本才能建立全盘索引。

---
## 7. 复现步骤

```powershell
# 1. 分析
$env:GHIDRA_INSTALL_DIR = "$env:LOCALAPPDATA\rea-tools\ghidra\ghidra_12.1.4_PUBLIC"
$env:JAVA_HOME         = "$env:LOCALAPPDATA\rea-tools\jdk21\jdk-21.0.13+9"
$cli = "$env:LOCALAPPDATA\npm-cache\_npx\dea9475797100b20\node_modules\rea-agents\scripts\rea.mjs"
node $cli analyze "$exe" --provider ghidra --snapshot everything.reasnap --format json > overview.json

# 2. 验证索引(需管理员)
cargo run --release -p launcher-index --bin usn_probe -- report.txt
```
