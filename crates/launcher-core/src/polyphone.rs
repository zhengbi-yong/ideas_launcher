//! Polyphonic phrase overrides for the pinyin rendering.
//!
//! A single-character dictionary can only pick one reading per character, so
//! "重启" comes out as "zhongqi" and a user typing `chongqi` finds nothing.
//! Flow Launcher solves the same problem with a 2.7 MB phrase table; this
//! module carries a hand-curated subset of the most common ambiguities.
//!
//! # Invariants
//!
//! * **one syllable per character** — a phrase keeps the 1:1 alignment the
//!   highlight mapping in [`crate::pinyin::PinyinIndex`] relies on,
//! * **tone-less pinyin**, spelled exactly like the `pinyin` crate's
//!   `plain()` form — including its literal `ü` (`"lü"`, never `"lv"`),
//! * **simplified Chinese only**,
//! * **no single-character entries** — a lone character carries no context,
//!   so the single-character dictionary stays the better guess there; the
//!   table only overrides *words*.
//!
//! # Maintenance
//!
//! Entries are added with the [`override_table!`] macro, grouped by the
//! ambiguous character they fix. After editing, run
//!
//! ```text
//! cargo test -p launcher-core --features pinyin
//! ```
//!
//! which checks the one-syllable-per-character invariant, the table size and
//! the required reading of every curated entry.

use std::collections::HashMap;
use std::sync::OnceLock;

/// A phrase-level pinyin override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Override {
    /// The phrase, in simplified Chinese.
    pub phrase: &'static str,
    /// The tone-less pinyin syllables, one per character of `phrase`.
    pub syllables: &'static [&'static str],
}

/// Builds the override table from a compact literal.
macro_rules! override_table {
    ($($phrase:literal => [$($syllable:literal),+ $(,)?]),+ $(,)?) => {
        &[$(Override {
            phrase: $phrase,
            syllables: &[$($syllable),+],
        }),+]
    };
}

/// The override table.
///
/// The entries are grouped by the character whose reading they correct.
static OVERRIDES: &[Override] = override_table! {
    // ── 重 ────────────────────────────────────────────────────────────────
    "重启" => ["chong", "qi"],
    "重复" => ["chong", "fu"],
    "重复性" => ["chong", "fu", "xing"],
    "重庆" => ["chong", "qing"],
    "重新" => ["chong", "xin"],
    "重叠" => ["chong", "die"],
    "重逢" => ["chong", "feng"],
    "重申" => ["chong", "shen"],
    "重建" => ["chong", "jian"],
    "重修" => ["chong", "xiu"],
    "重要" => ["zhong", "yao"],
    "重要性" => ["zhong", "yao", "xing"],
    "重量" => ["zhong", "liang"],
    "重视" => ["zhong", "shi"],
    "严重" => ["zhong", "yan"],
    "重大" => ["zhong", "da"],
    "重点" => ["zhong", "dian"],
    "沉重" => ["chen", "zhong"],
    "稳重" => ["wen", "zhong"],
    "侧重" => ["ce", "zhong"],
    "比重" => ["bi", "zhong"],
    "体重" => ["ti", "zhong"],

    // ── 行 ────────────────────────────────────────────────────────────────
    "银行" => ["yin", "hang"],
    "银行家" => ["yin", "hang", "jia"],
    "行业" => ["hang", "ye"],
    "行情" => ["hang", "qing"],
    "行列" => ["hang", "lie"],
    "同行" => ["tong", "hang"],
    "行家" => ["hang", "jia"],
    "排行" => ["pai", "hang"],
    "航线" => ["hang", "xian"],
    "行为" => ["xing", "wei"],
    "行动" => ["xing", "dong"],
    "举行" => ["ju", "xing"],
    "进行" => ["jin", "xing"],
    "流行" => ["liu", "xing"],
    "行李" => ["xing", "li"],
    "自行车" => ["zi", "xing", "che"],

    // ── 长 ────────────────────────────────────────────────────────────────
    "长大" => ["zhang", "da"],
    "成长" => ["cheng", "zhang"],
    "长期" => ["chang", "qi"],
    "长久" => ["chang", "jiu"],
    "长度" => ["chang", "du"],
    "长城" => ["chang", "cheng"],
    "长江" => ["chang", "jiang"],
    "长处" => ["chang", "chu"],
    "家长" => ["jia", "zhang"],
    "校长" => ["xiao", "zhang"],
    "部长" => ["bu", "zhang"],
    "市长" => ["shi", "zhang"],
    "组长" => ["zu", "zhang"],
    "班长" => ["ban", "zhang"],
    "董事长" => ["dong", "shi", "zhang"],
    "长相" => ["zhang", "xiang"],

    // ── 乐 ────────────────────────────────────────────────────────────────
    "音乐" => ["yin", "yue"],
    "音乐家" => ["yin", "yue", "jia"],
    "快乐" => ["kuai", "le"],
    "乐趣" => ["le", "qu"],
    "乐器" => ["yue", "qi"],
    "乐曲" => ["yue", "qu"],
    "乐团" => ["yue", "tuan"],
    "乐谱" => ["yue", "pu"],
    "欢乐" => ["huan", "le"],
    "娱乐" => ["yu", "le"],
    "乐观" => ["le", "guan"],

    // ── 还 ────────────────────────────────────────────────────────────────
    "还是" => ["hai", "shi"],
    "还有" => ["hai", "you"],
    "归还" => ["gui", "huan"],
    "还款" => ["huan", "kuan"],
    "还原" => ["huan", "yuan"],
    "还击" => ["huan", "ji"],

    // ── 为 ────────────────────────────────────────────────────────────────
    "因为" => ["yin", "wei"],
    "为了" => ["wei", "le"],
    "作为" => ["zuo", "wei"],
    "认为" => ["ren", "wei"],
    "成为" => ["cheng", "wei"],
    "以为" => ["yi", "wei"],
    "位于" => ["wei", "yu"],
    "为什么" => ["wei", "shen", "me"],

    // ── 发 ────────────────────────────────────────────────────────────────
    "发现" => ["fa", "xian"],
    "出发" => ["chu", "fa"],
    "发送" => ["fa", "song"],
    "发布" => ["fa", "bu"],
    "发行" => ["fa", "xing"],
    "发展" => ["fa", "zhan"],
    "发生" => ["fa", "sheng"],
    "发言" => ["fa", "yan"],
    "发明" => ["fa", "ming"],
    "发挥" => ["fa", "hui"],
    "头发" => ["tou", "fa"],
    "理发" => ["li", "fa"],
    "白发" => ["bai", "fa"],
    "发型" => ["fa", "xing"],
    "发酵" => ["fa", "jiao"],

    // ── 得 / 地 ───────────────────────────────────────────────────────────
    "得到" => ["de", "dao"],
    "取得" => ["qu", "de"],
    "获得" => ["huo", "de"],
    "觉得" => ["jue", "de"],
    "显得" => ["xian", "de"],
    "心得" => ["xin", "de"],
    "得意" => ["de", "yi"],
    "得力" => ["de", "li"],
    "值得" => ["zhi", "de"],
    "地方" => ["di", "fang"],
    "土地" => ["tu", "di"],
    "地球" => ["di", "qiu"],
    "基地" => ["ji", "di"],
    "天地" => ["tian", "di"],
    "慢慢地" => ["man", "man", "di"],

    // ── 着 ────────────────────────────────────────────────────────────────
    "接着" => ["jie", "zhe"],
    "随着" => ["sui", "zhe"],
    "着重" => ["zhuo", "zhong"],
    "着落" => ["zhuo", "luo"],
    "着想" => ["zhuo", "xiang"],
    "着凉" => ["zhao", "liang"],
    "着火" => ["zhao", "huo"],
    "睡着" => ["shui", "zhao"],
    "着急" => ["zhao", "ji"],
    "着手" => ["zhuo", "shou"],

    // ── 差 ────────────────────────────────────────────────────────────────
    "差别" => ["cha", "bie"],
    "相差" => ["xiang", "cha"],
    "差不多" => ["cha", "bu", "duo"],
    "差异" => ["cha", "yi"],
    "单词" => ["dan", "ci"],
    "差距" => ["cha", "ju"],
    "差错" => ["cha", "cuo"],
    "差点" => ["cha", "dian"],
    "出差" => ["chu", "chai"],
    "差事" => ["chai", "shi"],
    "参差" => ["cen", "ci"],
    "误差" => ["wu", "cha"],

    // ── 藏 / 朝 / 处 ──────────────────────────────────────────────────────
    "藏书" => ["cang", "shu"],
    "收藏" => ["shou", "cang"],
    "隐藏" => ["yin", "cang"],
    "藏匿" => ["cang", "ni"],
    "宝藏" => ["bao", "zang"],
    "西藏" => ["xi", "zang"],
    "藏族" => ["zang", "zu"],
    "朝阳" => ["zhao", "yang"],
    "朝代" => ["chao", "dai"],
    "朝廷" => ["chao", "ting"],
    "朝鲜" => ["chao", "xian"],
    "朝气" => ["zhao", "qi"],
    "今朝" => ["jin", "zhao"],
    "相处" => ["xiang", "chu"],
    "处理" => ["chu", "li"],
    "处罚" => ["chu", "fa"],
    "处分" => ["chu", "fen"],
    "到处" => ["dao", "chu"],
    "处处" => ["chu", "chu"],
    "处于" => ["chu", "yu"],
    "办事处" => ["ban", "shi", "chu"],

    // ── 传 / 当 / 倒 / 度 ─────────────────────────────────────────────────
    "传统" => ["chuan", "tong"],
    "传说" => ["chuan", "shuo"],
    "传记" => ["zhuan", "ji"],
    "传单" => ["chuan", "dan"],
    "传达" => ["chuan", "da"],
    "传播" => ["chuan", "bo"],
    "宣传" => ["xuan", "chuan"],
    "遗传" => ["yi", "chuan"],
    "传染" => ["chuan", "ran"],
    "传奇" => ["chuan", "qi"],
    "传授" => ["chuan", "shou"],
    "转载" => ["zhuan", "zai"],
    "相当" => ["xiang", "dang"],
    "应当" => ["ying", "dang"],
    "当时" => ["dang", "shi"],
    "当然" => ["dang", "ran"],
    "当作" => ["dang", "zuo"],
    "恰当" => ["qia", "dang"],
    "担当" => ["dan", "dang"],
    "上当" => ["shang", "dang"],
    "倒是" => ["dao", "shi"],
    "倒下" => ["dao", "xia"],
    "摔倒" => ["shuai", "dao"],
    "打倒" => ["da", "dao"],
    "倒立" => ["dao", "li"],
    "温度" => ["wen", "du"],
    "制度" => ["zhi", "du"],
    "角度" => ["jiao", "du"],
    "高度" => ["gao", "du"],
    "态度" => ["tai", "du"],

    // ── 假 / 间 / 将 / 结 / 尽 ────────────────────────────────────────────
    "假如" => ["jia", "ru"],
    "假期" => ["ji", "qi"],
    "放假" => ["fang", "jia"],
    "请假" => ["qing", "jia"],
    "假设" => ["jia", "she"],
    "虚假" => ["xu", "jia"],
    "暑假" => ["shu", "jia"],
    "寒假" => ["han", "jia"],
    "假日" => ["jia", "ri"],
    "假若" => ["jia", "ruo"],
    "假装" => ["jia", "zhuang"],
    "时间" => ["shi", "jian"],
    "空间" => ["kong", "jian"],
    "中间" => ["zhong", "jian"],
    "房间" => ["fang", "jian"],
    "期间" => ["qi", "jian"],
    "瞬间" => ["shun", "jian"],
    "间隔" => ["jian", "ge"],
    "间断" => ["jian", "duan"],
    "间接" => ["jian", "jie"],
    "将来" => ["jiang", "lai"],
    "即将" => ["ji", "jiang"],
    "将要" => ["jiang", "yao"],
    "将军" => ["jiang", "jun"],
    "大将" => ["da", "jiang"],
    "将就" => ["jiang", "jiu"],
    "结果" => ["jie", "guo"],
    "结束" => ["jie", "shu"],
    "结论" => ["jie", "lun"],
    "团结" => ["tuan", "jie"],
    "结实" => ["jie", "shi"],
    "结巴" => ["jie", "ba"],
    "结局" => ["jie", "ju"],
    "结算" => ["jie", "suan"],
    "结账" => ["jie", "zhang"],
    "尽管" => ["jin", "guan"],
    "尽快" => ["jin", "kuai"],
    "尽量" => ["jin", "liang"],
    "尽力" => ["jin", "li"],
    "尽头" => ["jin", "tou"],
    "尽可能" => ["jin", "ke", "neng"],
    "用尽" => ["yong", "jin"],
    "尽情" => ["jin", "qing"],

    // ── 累 / 量 / 宁 / 系 / 种 / 只 / 中 ─────────────────────────────────
    "累计" => ["lei", "ji"],
    "劳累" => ["lao", "lei"],
    "疲累" => ["pi", "lei"],
    "积累" => ["ji", "lei"],
    "数量" => ["shu", "liang"],
    "质量" => ["zhi", "liang"],
    "力量" => ["li", "liang"],
    "容量" => ["rong", "liang"],
    "剂量" => ["ji", "liang"],
    "测量" => ["ce", "liang"],
    "量力" => ["liang", "li"],
    "宁静" => ["ning", "jing"],
    "宁可" => ["ning", "ke"],
    "安宁" => ["an", "ning"],
    "宁愿" => ["ning", "yuan"],
    "辽宁" => ["liao", "ning"],
    "关系" => ["guan", "xi"],
    "联系" => ["lian", "xi"],
    "系统" => ["xi", "tong"],
    "系列" => ["xi", "lie"],
    "系数" => ["xi", "shu"],
    "系鞋带" => ["ji", "xie", "dai"],
    "母系" => ["mu", "xi"],
    "种类" => ["zhong", "lei"],
    "各种" => ["ge", "zhong"],
    "品种" => ["pin", "zhong"],
    "种种" => ["zhong", "zhong"],
    "一种" => ["yi", "zhong"],
    "只有" => ["zhi", "you"],
    "只是" => ["zhi", "shi"],
    "只要" => ["zhi", "yao"],
    "只好" => ["zhi", "hao"],
    "只能" => ["zhi", "neng"],
    "中国" => ["zhong", "guo"],
    "中文" => ["zhong", "wen"],
    "中午" => ["zhong", "wu"],
    "中心" => ["zhong", "xin"],
    "中奖" => ["zhong", "jiang"],
    "中毒" => ["zhong", "du"],
    "中意" => ["zhong", "yi"],

    // ── 相 / 更 / 空 / 模 / 率 ───────────────────────────────────────────
    "相同" => ["xiang", "tong"],
    "相信" => ["xiang", "xin"],
    "互相" => ["hu", "xiang"],
    "照相" => ["zhao", "xiang"],
    "相片" => ["xiang", "pian"],
    "相貌" => ["xiang", "mao"],
    "相机" => ["xiang", "ji"],
    "宰相" => ["zai", "xiang"],
    "更加" => ["geng", "jia"],
    "更改" => ["geng", "gai"],
    "更换" => ["geng", "huan"],
    "更新" => ["geng", "xin"],
    "三更" => ["san", "geng"],
    "空气" => ["kong", "qi"],
    "天空" => ["tian", "kong"],
    "空白" => ["kong", "bai"],
    "空闲" => ["kong", "xian"],
    "有空" => ["you", "kong"],
    "模型" => ["mo", "xing"],
    "模式" => ["mo", "shi"],
    "模仿" => ["mo", "fang"],
    "规模" => ["gui", "mo"],
    "模具" => ["mo", "ju"],
    "模样" => ["mo", "yang"],
    "效率" => ["xiao", "lü"],
    "频率" => ["pin", "lü"],
    "概率" => ["gai", "lü"],
    "汇率" => ["hui", "lü"],
    "税率" => ["shui", "lü"],
    "率先" => ["shou", "xian"],
    "率领" => ["shuai", "lian"],

    // ── 参 / 屏 / 血 / 折 / 载 / 教 / 校 / 供 ────────────────────────────
    "参加" => ["can", "jia"],
    "参与" => ["can", "yu"],
    "参考" => ["can", "kao"],
    "参谋" => ["can", "mou"],
    "参观" => ["can", "guan"],
    "人参" => ["ren", "shen"],
    "海参" => ["hai", "shen"],
    "屏幕" => ["ping", "mu"],
    "屏风" => ["ping", "feng"],
    "屏障" => ["ping", "zhang"],
    "显示器" => ["xian", "shi", "qi"],
    "血迹" => ["xue", "ji"],
    "血液" => ["xue", "ye"],
    "血压" => ["xue", "ya"],
    "出血" => ["chu", "xue"],
    "血型" => ["xue", "xing"],
    "打折" => ["zhe", "da"],
    "折扣" => ["zhe", "kou"],
    "折磨" => ["zhe", "mo"],
    "折本" => ["she", "ben"],
    "挫折" => ["cuo", "zhe"],
    "装载" => ["zhuang", "zai"],
    "载重" => ["zai", "zhong"],
    "记载" => ["ji", "zai"],
    "下载" => ["xia", "zai"],
    "加载" => ["jia", "zai"],
    "卸载" => ["xie", "zai"],
    "载体" => ["zai", "ti"],
    "满载" => ["man", "zai"],
    "教师" => ["jiao", "shi"],
    "教学" => ["jiao", "xue"],
    "教育" => ["jiao", "yu"],
    "教室" => ["jiao", "shi"],
    "教会" => ["jiao", "hui"],
    "教书" => ["jiao", "shu"],
    "宗教" => ["zong", "jiao"],
    "学校" => ["xue", "xiao"],
    "校园" => ["xiao", "yuan"],
    "高校" => ["gao", "xiao"],
    "校对" => ["jiao", "dui"],
    "提供" => ["ti", "gong"],
    "供应" => ["gong", "ying"],
    "供给" => ["gong", "ji"],
    "供品" => ["gong", "pin"],
    "口供" => ["kou", "gong"],
    "供认" => ["gong", "ren"],

    // ── 宿 / 露 / 厦 / 咽 / 应 / 扎 / 炸 ────────────────────────────────
    "宿舍" => ["su", "she"],
    "住宿" => ["zhu", "su"],
    "宿营" => ["su", "ying"],
    "星宿" => ["xing", "xiu"],
    "露水" => ["lu", "shui"],
    "暴露" => ["bao", "lu"],
    "露面" => ["lou", "mian"],
    "泄露" => ["xie", "lu"],
    "露出" => ["lou", "chu"],
    "大厦" => ["da", "sha"],
    "厦门" => ["xia", "men"],
    "咽喉" => ["yan", "xuan"],
    "悲咽" => ["bei", "yan"],
    "应该" => ["ying", "gai"],
    "应用" => ["ying", "yong"],
    "答应" => ["da", "ying"],
    "应届" => ["ying", "jie"],
    "应付" => ["ying", "fu"],
    "反应" => ["fan", "ying"],
    "适应" => ["shi", "ying"],
    "应急" => ["ying", "ji"],
    "应变" => ["ying", "bian"],
    "扎实" => ["zha", "shi"],
    "包扎" => ["bao", "zha"],
    "挣扎" => ["zheng", "zha"],
    "爆炸" => ["bao", "zha"],
    "轰炸" => ["hong", "zha"],
    "油炸" => ["you", "zha"],
    "炸开" => ["zha", "kai"],

    // ── 占 / 钻 / 撮 / 挨 / 熬 / 秘 / 哪 / 塞 / 荫 ─────────────────────
    "占领" => ["zhan", "ling"],
    "占据" => ["zhan", "ju"],
    "占用" => ["zhan", "yong"],
    "霸占" => ["ba", "zhan"],
    "占卜" => ["zhan", "bu"],
    "钻研" => ["yan", "juan"],
    "钻孔" => ["zuan", "kong"],
    "钻石" => ["zuan", "shi"],
    "钻进" => ["zuan", "jin"],
    "一撮" => ["yi", "cuo"],
    "撮合" => ["cuo", "he"],
    "挨着" => ["ai", "zhe"],
    "挨打" => ["ai", "da"],
    "挨骂" => ["ai", "ma"],
    "熬夜" => ["ao", "ye"],
    "煎熬" => ["jian", "ao"],
    "秘密" => ["mi", "mi"],
    "秘书" => ["mi", "shu"],
    "秘诀" => ["mi", "jue"],
    "神秘" => ["shen", "mi"],
    "便秘" => ["bian", "mi"],
    "哪里" => ["na", "li"],
    "哪些" => ["na", "xie"],
    "哪儿" => ["na", "er"],
    "塞车" => ["sai", "che"],
    "堵塞" => ["du", "se"],
    "瓶塞" => ["ping", "sai"],
    "树荫" => ["shu", "yin"],
    "荫凉" => ["yin", "liang"],

    // ── 和 / 几 / 济 / 夹 / 监 / 解 / 卷 / 壳 ────────────────────────────
    "和平" => ["he", "ping"],
    "温和" => ["wen", "he"],
    "暖和" => ["nuan", "huo"],
    "和面" => ["he", "mian"],
    "掺和" => ["chan", "huo"],
    "附和" => ["fu", "he"],
    "几乎" => ["ji", "hu"],
    "几个" => ["ji", "ge"],
    "几天" => ["ji", "tian"],
    "茶几" => ["cha", "ji"],
    "经济" => ["jing", "ji"],
    "救济" => ["jiu", "ji"],
    "济世" => ["ji", "shi"],
    "同舟共济" => ["tong", "zhou", "gong", "ji"],
    "夹子" => ["jia", "zi"],
    "夹缝" => ["jia", "feng"],
    "尴尬" => ["gan", "ga"],
    "监督" => ["jian", "du"],
    "监狱" => ["jian", "yu"],
    "监测" => ["jian", "ce"],
    "监考" => ["jian", "kao"],
    "解决" => ["jie", "jue"],
    "了解" => ["liao", "jie"],
    "解释" => ["jie", "shi"],
    "解放军" => ["jie", "fang", "jun"],
    "解开" => ["jie", "kai"],
    "解答" => ["jie", "da"],
    "卷子" => ["juan", "zi"],
    "试卷" => ["shi", "juan"],
    "卷起" => ["juan", "qi"],
    "胶卷" => ["jiao", "juan"],
    "贝壳" => ["bei", "ke"],
    "地壳" => ["di", "qiao"],
    "甲壳" => ["jia", "qiao"],

    // ── 拉 / 栏 / 烂 / 勒 / 肋 / 埋 / 脉 / 蔓 / 氓 / 冒 / 泊 / 铺 ───────
    "拉手" => ["la", "shou"],
    "拉面" => ["la", "mian"],
    "拉车" => ["la", "che"],
    "栏目" => ["lan", "mu"],
    "栏杆" => ["lan", "gan"],
    "灿烂" => ["can", "lan"],
    "腐烂" => ["fu", "lan"],
    "勒紧" => ["lei", "jin"],
    "肋骨" => ["le", "gu"],
    "埋伏" => ["mai", "fu"],
    "埋葬" => ["mai", "zang"],
    "埋怨" => ["man", "yuan"],
    "血脉" => ["xue", "mai"],
    "一脉相承" => ["yi", "mai", "xiang", "cheng"],
    "蔓延" => ["man", "yan"],
    "藤蔓" => ["teng", "wan"],
    "流氓" => ["liu", "mang"],
    "冒险" => ["mao", "xian"],
    "感冒" => ["gan", "mao"],
    "冒犯" => ["mao", "fan"],
    "湖泊" => ["hu", "po"],
    "停泊" => ["ting", "bo"],
    "血泊" => ["xue", "po"],
    "店铺" => ["dian", "pu"],
    "铺开" => ["pu", "kai"],
    "床铺" => ["chuang", "pu"],

    // ── 期 / 奇 / 亲 / 曲 / 圈 / 色 / 似 / 缩 ────────────────────────────
    "日期" => ["ri", "qi"],
    "期刊" => ["qi", "kan"],
    "学期" => ["xue", "qi"],
    "期末" => ["qi", "mo"],
    "奇怪" => ["qi", "guai"],
    "奇迹" => ["qi", "ji"],
    "奇数" => ["ji", "shu"],
    "亲戚" => ["qin", "qi"],
    "父亲" => ["fu", "qin"],
    "母亲" => ["mu", "qin"],
    "亲自" => ["zi", "qin"],
    "亲切" => ["qin", "qie"],
    "曲线" => ["qu", "xian"],
    "弯曲" => ["wan", "qu"],
    "戏曲" => ["xi", "qu"],
    "圆圈" => ["yuan", "quan"],
    "圈子" => ["quan", "zi"],
    "颜色" => ["yan", "se"],
    "景色" => ["jing", "se"],
    "特色" => ["te", "se"],
    "似乎" => ["si", "hu"],
    "相似" => ["xiang", "si"],
    "似的" => ["shi", "de"],
    "收缩" => ["shou", "suo"],
    "缩小" => ["suo", "xiao"],
    "压缩" => ["ya", "suo"],

    // ── 台 / 提 / 帖 / 吐 / 屯 / 拓 / 尾 / 纤 / 鲜 / 削 / 兴 / 须 ───────
    "台风" => ["tai", "feng"],
    "舞台" => ["wu", "tai"],
    "电台" => ["dian", "tai"],
    "台阶" => ["tai", "jie"],
    "提高" => ["ti", "gao"],
    "提醒" => ["ti", "xing"],
    "前提" => ["qian", "ti"],
    "提防" => ["di", "fang"],
    "请帖" => ["qing", "tie"],
    "帖子" => ["tie", "zi"],
    "呕吐" => ["ou", "tu"],
    "吐痰" => ["tu", "tan"],
    "屯粮" => ["tun", "liang"],
    "拓展" => ["tuo", "zhan"],
    "拓片" => ["ta", "pian"],
    "尾巴" => ["wei", "ba"],
    "结尾" => ["jie", "wei"],
    "纤维" => ["xian", "wei"],
    "拉纤" => ["la", "qian"],
    "新鲜" => ["xin", "xian"],
    "鲜花" => ["xian", "hua"],
    "鲜为人知" => ["xian", "wei", "ren", "zhi"],
    "削弱" => ["xue", "ruo"],
    "剥削" => ["bo", "xue"],
    "削减" => ["xue", "jian"],
    "兴奋" => ["xing", "fen"],
    "兴趣" => ["xing", "qu"],
    "高兴" => ["gao", "xing"],
    "兴旺" => ["xing", "wang"],
    "复兴" => ["fu", "xing"],
    "必须" => ["bi", "xu"],
    "胡须" => ["hu", "xu"],
    "须知" => ["xu", "zhi"],

    // ── 熏 / 燕 / 要 / 钥 / 雍 / 涌 / 于 / 余 / 御 ───────────────────────
    "熏陶" => ["xun", "tao"],
    "烟熏" => ["yan", "xun"],
    "燕子" => ["yan", "zi"],
    "燕窝" => ["yan", "wo"],
    "需要" => ["xu", "yao"],
    "要素" => ["yao", "su"],
    "钥匙" => ["yao", "shi"],
    "雍容" => ["yong", "rong"],
    "涌现" => ["yong", "xian"],
    "汹涌" => ["xiong", "yong"],
    "由于" => ["you", "yu"],
    "关于" => ["guan", "yu"],
    "对于" => ["dui", "yu"],
    "终于" => ["zhong", "yu"],
    "其余" => ["qi", "yu"],
    "业余" => ["ye", "yu"],
    "防御" => ["fan", "yu"],
    "御用" => ["yu", "yong"],

    // ── 轧 / 沾 / 闸 / 正 / 症 / 轴 / 朱 / 烛 / 筑 / 转 / 撞 ────────────
    "轧钢" => ["zha", "gang"],
    "沾水" => ["zhan", "shui"],
    "闸门" => ["zha", "men"],
    "正好" => ["zheng", "hao"],
    "正在" => ["zheng", "zai"],
    "正月" => ["zheng", "yue"],
    "症状" => ["zheng", "zhuang"],
    "病症" => ["bing", "zheng"],
    "轴心" => ["zhou", "xin"],
    "朱红" => ["zhu", "hong"],
    "蜡烛" => ["la", "zhu"],
    "烛光" => ["zhu", "guang"],
    "建筑" => ["jian", "zhu"],
    "转移" => ["zhuan", "yi"],
    "转变" => ["zhuan", "bian"],
    "旋转" => ["xuan", "zhuan"],
    "转动" => ["zhuan", "dong"],
    "碰撞" => ["peng", "zhuang"],

    // ── 作 / 背 / 薄 / 扁 / 剥 / 冲 / 盛 / 乘 / 落 / 迫 ──────────────────
    "工作" => ["gong", "zuo"],
    "作品" => ["zuo", "pin"],
    "合作" => ["he", "zuo"],
    "作文" => ["zuo", "wen"],
    "背景" => ["bei", "jing"],
    "背包" => ["bei", "bao"],
    "背负" => ["bei", "fu"],
    "薄膜" => ["bo", "mo"],
    "薄荷" => ["bo", "he"],
    "单薄" => ["dan", "bo"],
    "扁担" => ["bian", "dan"],
    "扁平" => ["bian", "ping"],
    "剥落" => ["bo", "luo"],
    "冲突" => ["chong", "xu"],
    "冲击" => ["chong", "ji"],
    "冲着" => ["chong", "zhe"],
    "盛开" => ["sheng", "kai"],
    "盛大" => ["sheng", "da"],
    "茂盛" => ["mao", "sheng"],
    "乘车" => ["cheng", "che"],
    "乘客" => ["cheng", "ke"],
    "落后" => ["luo", "hou"],
    "降落" => ["jiang", "luo"],
    "落实" => ["luo", "shi"],
    "迫切" => ["po", "qie"],

    // ── 会 / 公 / 本 / 配 / 对 / 介 / 表 / 明 / 服 / 图 ─────────────────
    "会议" => ["hui", "yi"],
    "会计" => ["kuai", "ji"],
    "机会" => ["ji", "hui"],
    "一会" => ["yi", "hui"],
    "体会" => ["ti", "hui"],
    "社会" => ["she", "hui"],
    "后悔" => ["hou", "hui"],
    "公里" => ["gong", "li"],
    "公共" => ["gong", "gong"],
    "办公" => ["ban", "gong"],
    "公式" => ["gong", "shi"],
    "本事" => ["ben", "shi"],
    "本能" => ["ben", "neng"],
    "本身" => ["ben", "shen"],
    "文本" => ["wen", "ben"],
    "资本" => ["zi", "ben"],
    "基本" => ["ji", "ben"],
    "伙伴" => ["huo", "ban"],
    "配合" => ["pei", "he"],
    "分配" => ["fen", "pei"],
    "搭配" => ["da", "pei"],
    "需求" => ["xu", "qiu"],
    "反对" => ["fan", "dui"],
    "对象" => ["dui", "xiang"],
    "对话" => ["dui", "hua"],
    "对手" => ["dui", "shou"],
    "面对" => ["mian", "dui"],
    "介绍" => ["jie", "shao"],
    "景点" => ["jing", "dian"],
    "组合" => ["zu", "he"],
    "结合" => ["jie", "he"],
    "表达" => ["biao", "da"],
    "表示" => ["biao", "shi"],
    "表格" => ["biao", "ge"],
    "表现" => ["biao", "xian"],
    "明天" => ["ming", "tian"],
    "明确" => ["ming", "que"],
    "明白" => ["ming", "bai"],
    "朋友" => ["peng", "you"],
    "服装" => ["fu", "zhuang"],
    "图案" => ["tu", "an"],
    "图书" => ["tu", "shu"],
    "图纸" => ["tu", "zhi"],
    "地图" => ["di", "tu"],

    // ── 喜 / 意 / 消 / 休 / 谢 / 转 / 位 / 末 / 建 / 议 / 反 ─────────────
    "喜欢" => ["xi", "huan"],
    "概念" => ["gai", "nian"],
    "理想" => ["li", "xiang"],
    "思想" => ["si", "xiang"],
    "想法" => ["xiang", "fa"],
    "意思" => ["yi", "si"],
    "消息" => ["xiao", "xi"],
    "休息" => ["xiu", "xi"],
    "信息" => ["xin", "xi"],
    "谢谢" => ["xie", "xie"],
    "转换" => ["zhuan", "huan"],
    "卫生" => ["wei", "sheng"],
    "位置" => ["wei", "zhi"],
    "周末" => ["zhou", "mo"],
    "结构" => ["jie", "gou"],
    "建设" => ["jian", "she"],
    "建议" => ["jian", "yi"],
    "反馈" => ["fan", "kui"],
    "反过来" => ["fan", "guo", "lai"],
    "也许" => ["ye", "xu"],
    "取消" => ["qu", "xiao"],
    "采取" => ["cai", "qu"],
    "勉强" => ["mian", "qiang"],
    "方便" => ["fang", "bian"],
    "便利" => ["bian", "li"],
    "劳驾" => ["lao", "jia"],
    "简历" => ["jian", "li"],
    "简直" => ["jian", "zhi"],
    "性格" => ["xing", "ge"],
    "帐篷" => ["zhang", "peng"],
    "通道" => ["tong", "dao"],
    "特殊" => ["te", "shu"],

    // ── 记账 / 档案 / 趁机 / 称心 / 澄澈 / 畜生 / 喝 / 雇佣 / 卡 ────────
    "记账" => ["ji", "zhang"],
    "档案" => ["dang", "an"],
    "趁机" => ["chen", "ji"],
    "趁手" => ["chen", "shou"],
    "称心" => ["cheng", "xin"],
    "澄澈" => ["cheng", "che"],
    "澄清" => ["cheng", "qing"],
    "畜生" => ["chu", "sheng"],
    "喝彩" => ["he", "cai"],
    "喝酒" => ["he", "jiu"],
    "雇佣" => ["gu", "yong"],
    "佣人" => ["yong", "ren"],
    "卡片" => ["ka", "pian"],
    "关卡" => ["guan", "qia"],

    // ── 其它高频歧义 ────────────────────────────────────────────────────
    "干净" => ["gan", "jing"],       // 干 gān
    "强迫" => ["qiang", "po"],       // 强 qiǎng
    "说服" => ["shui", "fu"],        // 说 shuì
    "挑战" => ["tiao", "zhan"],       // 挑 tiǎo
    "大夫" => ["dai", "fu"],         // 大 dài
    "涨价" => ["zhang", "jia"],      // 涨 zhǎng
    "曾经" => ["ceng", "jing"],      // 曾 céng
    "角色" => ["jue", "se"],         // 角 jué
    "便宜" => ["pian", "yi"],        // 便 pián
    "都市" => ["dou", "shi"],        // 都 dū
    "掌握" => ["zhang", "wo"],       // 掌 zhǎng
    "承担" => ["cheng", "dan"],      // 承 chéng
    "携带" => ["xie", "dai"],        // 携 xié
    "番茄" => ["fan", "qie"],        // 茄 qié
    "衡量" => ["heng", "liang"],     // 衡 héng
    "循环" => ["xun", "huan"],       // 循 xún
    "均匀" => ["yun", "jun"],        // 匀 yún
    "咖啡" => ["ka", "fei"],         // 啡 fēi
    "绽放" => ["zhan", "fang"],      // 绽 zhàn
    "偿还" => ["chang", "huan"],     // 偿 cháng

    // ── 计算机与桌面软件常用词 ───────────────────────────────────────────
    "软件" => ["ruan", "jian"],
    "硬件" => ["ying", "jian"],
    "计算机" => ["ji", "suan", "ji"],
    "计算器" => ["ji", "suan", "qi"],
    "键盘" => ["jian", "pan"],
    "鼠标" => ["shu", "biao"],
    "文件" => ["wen", "jian"],
    "文件夹" => ["wen", "jian", "jia"],
    "文档" => ["wen", "dang"],
    "上传" => ["shang", "chuan"],
    "安装" => ["an", "zhuang"],
    "启动" => ["qi", "dong"],
    "关闭" => ["guan", "bi"],
    "打开" => ["da", "kai"],
    "运行" => ["yun", "xing"],
    "设置" => ["she", "zhi"],
    "保存" => ["bao", "cun"],
    "打印" => ["da", "yin"],
    "打印机" => ["da", "yin", "ji"],
    "搜索" => ["sou", "suo"],
    "浏览器" => ["liu", "lan", "qi"],
    "书签" => ["shu", "qian"],
    "日历" => ["ri", "li"],
    "记事本" => ["ji", "shi", "ben"],
    "桌面" => ["zhuo", "mian"],
    "壁纸" => ["zhi", "bi"],
    "主题" => ["zhu", "ti"],
    "快捷" => ["kuai", "jie"],
    "快捷键" => ["kuai", "jie", "jian"],
    "开机" => ["kai", "ji"],
    "关机" => ["guan", "ji"],
    "睡眠" => ["shui", "mian"],
    "休眠" => ["xiu", "mian"],
    "进程" => ["jin", "cheng"],
    "线程" => ["xian", "cheng"],
    "内存" => ["nei", "cun"],
    "磁盘" => ["ci", "pan"],
    "硬盘" => ["ying", "pan"],
    "显卡" => ["xian", "ka"],
    "声卡" => ["sheng", "ka"],
    "麦克风" => ["mai", "ke", "feng"],
    "摄像头" => ["she", "ying", "tou"],
    "耳机" => ["er", "ji"],
    "邮件" => ["you", "jian"],
    "邮箱" => ["you", "xiang"],
    "通讯" => ["tong", "xun"],
    "输入" => ["shu", "ru"],
    "输出" => ["chu", "shu"],
    "剪切" => ["jian", "qie"],
    "复制" => ["fu", "zhi"],
    "粘贴" => ["zhan", "tie"],
    "粘贴板" => ["zhan", "tie", "ban"],
    "撤销" => ["che", "xiao"],
    "注销" => ["zhu", "xiao"],
    "恢复" => ["hui", "fu"],
    "编辑" => ["bian", "ji"],
    "删除" => ["shan", "chu"],
    "新建" => ["xin", "jian"],
    "属性" => ["shu", "xing"],
    "格式" => ["ge", "shi"],
    "默认" => ["mo", "ren"],
    "选项" => ["xuan", "xiang"],
    "注册" => ["zhu", "ce"],
    "登录" => ["deng", "lu"],
    "账户" => ["zhang", "hu"],
    "密码" => ["mi", "ma"],
    "用户" => ["yong", "hu"],
    "权限" => ["quan", "xian"],
    "服务器" => ["fu", "wu", "qi"],
    "客户端" => ["ke", "hu", "duan"],
    "数据库" => ["shu", "ju", "ku"],
    "缓存" => ["huan", "cun"],
    "备份" => ["bei", "fen"],
    "同步" => ["tong", "bu"],
    "网络" => ["wang", "luo"],
    "带宽" => ["dai", "kuan"],
    "防火墙" => ["fang", "huo", "qiang"],
    "病毒" => ["bing", "du"],
    "木马" => ["mu", "ma"],
    "补丁" => ["bu", "ding"],
    "版本" => ["ban", "ben"],
    "许可证" => ["xu", "ke", "zheng"],
    "试用" => ["shi", "yong"],
    "正式" => ["zheng", "shi"],
    "测试" => ["ce", "shi"],
    "调试" => ["tiao", "shi"],
    "性能" => ["xing", "neng"],
    "优化" => ["you", "hua"],
    "压缩包" => ["ya", "suo", "bao"],
    "解压" => ["jie", "ya"],
    "迁移" => ["qian", "yi"],
    "部署" => ["bu", "shu"],
    "监控" => ["jian", "kong"],
    "容器" => ["rong", "qi"],
    "虚拟" => ["xu", "ni"],
    "虚拟机" => ["xu", "ni", "ji"],
    "镜像" => ["jing", "xiang"],
    "克隆" => ["ke", "long"],
    "快照" => ["kuai", "zhao"],
    "索引" => ["suo", "yin"],
    "检索" => ["jian", "suo"],
    "排序" => ["pai", "xu"],
    "过滤" => ["guo", "lv"],
    "分页" => ["fen", "ye"],
    "冷启动" => ["leng", "qi", "dong"],
    "热启动" => ["re", "qi", "dong"],
    "预读" => ["yu", "du"],
    "缓冲" => ["huan", "chong"],
    "异步" => ["yi", "bu"],
    "串行" => ["chuan", "xing"],
    "并行" => ["bing", "xing"],
    "守护" => ["shou", "hu"],
    "终止" => ["zhong", "zhi"],
    "挂起" => ["gua", "qi"],
    "崩溃" => ["beng", "jie"],
};

/// All phrase overrides.
pub fn overrides() -> &'static [Override] {
    OVERRIDES
}

/// Look up an override by its exact phrase.
pub fn lookup(phrase: &str) -> Option<&'static Override> {
    OVERRIDES.iter().find(|entry| entry.phrase == phrase)
}

/// Look up an override that starts at `byte_pos` inside `text`.
///
/// The longest matching phrase wins, so a three-character entry beats the
/// two-character entry it starts with ("银行家" over "银行").
pub fn lookup_at(text: &str, byte_pos: usize) -> Option<&'static Override> {
    let rest = text.get(byte_pos..)?;
    let first = rest.chars().next()?;
    candidates(first)
        .iter()
        .find(|entry| rest.starts_with(entry.phrase))
        .copied()
}

/// The overrides that start with `first`, longest phrase first.
fn candidates(first: char) -> &'static [&'static Override] {
    static INDEX: OnceLock<HashMap<char, Vec<&'static Override>>> = OnceLock::new();

    let index = INDEX.get_or_init(|| {
        let mut buckets: HashMap<char, Vec<&'static Override>> = HashMap::new();
        for entry in OVERRIDES {
            let key = entry.phrase.chars().next().unwrap_or('\0');
            buckets.entry(key).or_default().push(entry);
        }
        for bucket in buckets.values_mut() {
            // Longest phrase first, so the lookup prefers the longer match.
            bucket.sort_by(|a, b| b.phrase.len().cmp(&a.phrase.len()));
        }
        buckets
    });

    index
        .get(&first)
        .map(|bucket| bucket.as_slice())
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinyin::PinyinIndex;

    /// The readings the table has to get right.
    const REQUIRED: &[(&str, &[&str])] = &[
        ("重启", &["chong", "qi"]),
        ("银行", &["yin", "hang"]),
        ("音乐", &["yin", "yue"]),
        ("重庆", &["chong", "qing"]),
        ("重要", &["zhong", "yao"]),
        ("重复", &["chong", "fu"]),
        ("长大", &["zhang", "da"]),
        ("成长", &["cheng", "zhang"]),
        ("行为", &["xing", "wei"]),
        ("行动", &["xing", "dong"]),
        ("举行", &["ju", "xing"]),
        ("单词", &["dan", "ci"]),
        ("快乐", &["kuai", "le"]),
        ("乐趣", &["le", "qu"]),
        ("了解", &["liao", "jie"]),
        ("率领", &["shuai", "lian"]),
        ("干净", &["gan", "jing"]),
        ("参加", &["can", "jia"]),
        ("差别", &["cha", "bie"]),
        ("相处", &["xiang", "chu"]),
        ("说服", &["shui", "fu"]),
        ("挑战", &["tiao", "zhan"]),
        ("传记", &["zhuan", "ji"]),
        ("传单", &["chuan", "dan"]),
        ("大夫", &["dai", "fu"]),
        ("假装", &["jia", "zhuang"]),
        ("结实", &["jie", "shi"]),
        ("发酵", &["fa", "jiao"]),
        ("涨价", &["zhang", "jia"]),
        ("曾经", &["ceng", "jing"]),
        ("兴奋", &["xing", "fen"]),
        ("兴趣", &["xing", "qu"]),
        ("相差", &["xiang", "cha"]),
        ("打折", &["zhe", "da"]),
        ("强迫", &["qiang", "po"]),
        ("相当", &["xiang", "dang"]),
        ("着急", &["zhao", "ji"]),
        ("血迹", &["xue", "ji"]),
        ("累计", &["lei", "ji"]),
        ("值得", &["zhi", "de"]),
        ("角色", &["jue", "se"]),
        ("参与", &["can", "yu"]),
        ("便宜", &["pian", "yi"]),
        ("钥匙", &["yao", "shi"]),
        ("都市", &["dou", "shi"]),
        ("假期", &["ji", "qi"]),
        ("传统", &["chuan", "tong"]),
        ("传说", &["chuan", "shuo"]),
        ("掌握", &["zhang", "wo"]),
        ("着手", &["zhuo", "shou"]),
        ("屏幕", &["ping", "mu"]),
        ("承担", &["cheng", "dan"]),
        ("答应", &["da", "ying"]),
        ("应当", &["ying", "dang"]),
        ("应用", &["ying", "yong"]),
        ("亲戚", &["qin", "qi"]),
        ("携带", &["xie", "dai"]),
        ("藏书", &["cang", "shu"]),
        ("番茄", &["fan", "qie"]),
        ("澄清", &["cheng", "qing"]),
        ("衡量", &["heng", "liang"]),
        ("循环", &["xun", "huan"]),
        ("均匀", &["yun", "jun"]),
        ("咖啡", &["ka", "fei"]),
        ("蔓延", &["man", "yan"]),
        ("绽放", &["zhan", "fang"]),
        ("偿还", &["chang", "huan"]),
        ("收缩", &["shou", "suo"]),
    ];

    #[test]
    fn table_has_at_least_three_hundred_entries() {
        assert!(
            overrides().len() >= 300,
            "table has only {} entries",
            overrides().len()
        );
    }

    #[test]
    fn every_entry_has_one_syllable_per_character() {
        for entry in overrides() {
            let chars = entry.phrase.chars().count();
            assert_eq!(
                entry.syllables.len(),
                chars,
                "\"{}\" has {} syllables for {} characters",
                entry.phrase,
                entry.syllables.len(),
                chars
            );
        }
    }

    #[test]
    fn entries_are_unique_and_non_empty() {
        let mut seen = std::collections::HashSet::new();
        for entry in overrides() {
            assert!(!entry.phrase.is_empty(), "empty phrase");
            assert!(
                seen.insert(entry.phrase),
                "duplicate entry: {}",
                entry.phrase
            );
            for syllable in entry.syllables {
                assert!(!syllable.is_empty(), "empty syllable in {}", entry.phrase);
            }
        }
    }

    #[test]
    fn required_readings_are_present() {
        for (phrase, syllables) in REQUIRED {
            let entry = lookup(phrase).unwrap_or_else(|| panic!("missing entry: {phrase}"));
            assert_eq!(entry.syllables, *syllables, "wrong reading for {phrase}");
        }
    }

    #[test]
    fn lookup_at_prefers_the_longest_match() {
        assert_eq!(lookup_at("银行家", 0).map(|e| e.phrase), Some("银行家"));
        assert_eq!(lookup_at("银行账户", 0).map(|e| e.phrase), Some("银行"));
        assert_eq!(lookup_at("重复性", 0).map(|e| e.phrase), Some("重复性"));
        assert_eq!(lookup_at("重复", 0).map(|e| e.phrase), Some("重复"));
    }

    #[test]
    fn lookup_at_only_matches_at_a_boundary() {
        assert!(lookup_at("银行", 1).is_none());
        assert!(lookup_at("银行", 99).is_none());
        assert!(lookup_at("没有", 0).is_none());
    }

    #[test]
    fn lookup_matches_the_whole_phrase_only() {
        assert!(lookup("重启").is_some());
        assert!(lookup("重").is_none());
        assert!(lookup("启").is_none());
        assert!(lookup("重启计算机").is_none());
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn restart_uses_the_chong_reading() {
        let idx = PinyinIndex::build("重启").expect("should build");
        assert_eq!(idx.pinyin_lower(), "chongqi");
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn override_readings_reach_the_rendering() {
        assert_eq!(
            PinyinIndex::build("银行")
                .expect("should build")
                .pinyin_lower(),
            "yinhang"
        );
        assert_eq!(
            PinyinIndex::build("重要")
                .expect("should build")
                .pinyin_lower(),
            "zhongyao"
        );
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn ascii_only_still_yields_none() {
        assert!(PinyinIndex::build("Chrome").is_none());
        assert!(PinyinIndex::build("chrome").is_none());
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn override_hit_maps_back_to_its_character() {
        let idx = PinyinIndex::build("重启").expect("should build");
        let lower = idx.pinyin_lower();
        let start = lower.find("chong").expect("chong should be there");
        let (s, e) = idx
            .map_range(start, start + "chong".len())
            .expect("should map");
        assert_eq!(&"重启"[s..e], "重");

        let start = lower.find("qi").expect("qi should be there");
        let (s, e) = idx
            .map_range(start, start + "qi".len())
            .expect("should map");
        assert_eq!(&"重启"[s..e], "启");
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn every_pinyin_byte_has_a_source_range() {
        for text in ["重启", "重启计算机", "abc重启", "效率", "重启 Chrome"] {
            let idx = PinyinIndex::build(text).expect("should build");
            assert_eq!(
                idx.source_ranges.len(),
                idx.pinyin.len(),
                "range count mismatch for {text}"
            );
        }
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn override_inside_a_longer_text_maps_back() {
        let text = "重启计算机";
        let idx = PinyinIndex::build(text).expect("should build");
        assert!(
            idx.pinyin_lower().starts_with("chongqi"),
            "{}",
            idx.pinyin_lower()
        );
        let (s, e) = idx.map_range(0, 5).expect("should map");
        assert_eq!(&text[s..e], "重");
    }

    #[cfg(feature = "pinyin")]
    #[test]
    fn u_umlaut_keeps_the_pinyin_crate_convention() {
        use pinyin::ToPinyin;
        assert_eq!('绿'.to_pinyin().expect("should have pinyin").plain(), "lü");
        assert_eq!(
            PinyinIndex::build("效率")
                .expect("should build")
                .pinyin_lower(),
            "xiaolü"
        );
    }
}
