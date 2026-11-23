# `runtime/` — Flow Launcher 插件运行时库

本目录提供**插件侧**需要的运行时库,让社区现成的 Flow Launcher 插件(主要是 Python 插件)
不用自己实现 JSON-RPC 线路协议,就能直接被我们的 Rust 宿主加载运行。

宿主侧(Rust)的职责是:拉起插件进程、说 JSON-RPC、把结果渲染出来。
插件侧(本目录)的职责是:把宿主发来的 JSON-RPC 翻译成插件作者熟悉的回调。

```
runtime/
  python/
    python_plugin.py        # 主 helper:Plugin / FlowLauncher 基类、Result、Query
    flowlauncher_api.py     # 宿主 API 封装(FlowLauncherAPI)
    jsonrpc_transport.py    # JSON-RPC 2.0 线路协议实现(按行分隔)
    flowlauncher.py         # 兼容 shim:让 `from flowlauncher import FlowLauncher` 可用
    selftest_plugin.py      # 最小示例插件(端到端自检用)
    selftest.py             # 端到端自检脚本(PASS/FAIL + 退出码)
  README.md
```

---

## 1. 上游出处与许可

### 1.1 官方 helper 库(我们移植的基准)

| 项 | 内容 |
|---|---|
| 仓库 | <https://github.com/Flow-Launcher/Flow.Launcher.JsonRPC.Python> |
| PyPI | <https://pypi.org/project/flowlauncher/>(包名 `flowlauncher`,最新发布版 0.2.0) |
| 许可 | **MIT**(仓库 `LICENSE`;README 注明部分代码源自 Wox 的 `JsonRPC/wox.py`,同为 MIT) |
| 官方示例插件 | <https://github.com/Flow-Launcher/Flow.Launcher.Plugin.HelloWorldPython> |

该仓库的结构是:`flowlauncher/FlowLauncher.py`(基类)、`flowlauncher/FlowLauncherAPI.py`(宿主 API)、
`flowlauncher/__init__.py`。**本目录的 `python_plugin.py` 与 `flowlauncher_api.py` 就是它的移植版**,
类名、方法名、回调签名、结果构造方式全部保持一致,只把「一次性 argv 传参 + 往 stdout 打印 JSON」
的旧线路换成了「stdin/stdout 常驻流 + JSON-RPC 2.0」。

### 1.2 协议侧的权威依据(我们 Rust 宿主要对齐的实现)

| 文件 | 作用 |
|---|---|
| `Flow.Launcher.Core/Plugin/JsonRPCPluginV2.cs` | V2 线路协议权威说明:宿主调 `initialize`/`query`/`context_menu`/`reload_data`/`close`,插件可反向调 `UpdateResults` |
| `Flow.Launcher.Core/Plugin/ProcessStreamPluginV2.cs` | 拉起插件进程:`PYTHONPATH` / `PYTHONDONTWRITEBYTECODE=1`、stdin/stdout 重定向、stderr 单独读 |
| `Flow.Launcher.Core/Plugin/PythonPluginV2.cs` | Python 插件启动方式(`python -c "<code>"` + `runpy`),以及 **`MessageHandlerType.NewLineDelimited`**(按行分隔) |
| `Flow.Launcher.Core/Plugin/NodePluginV2.cs` | Node 插件启动方式,以及 **`MessageHandlerType.HeaderDelimited`**(按头分隔) |
| `Flow.Launcher.Core/Plugin/JsonRPCV2Models/JsonRPCPublicAPI.cs` | 宿主暴露给插件的 API 方法清单 |
| `Flow.Launcher.Core/Plugin/JsonPRCModel.cs` | 请求/响应模型(`JsonRPCQueryResponseModel` 等) |
| `Flow.Launcher.Plugin/Result.cs` / `Query.cs` / `PluginInitContext.cs` | Result / Query / initialize 入参的字段定义 |

> 上游使用 `StreamJsonRpc 2.22.11`(`Flow.Launcher.Core/Flow.Launcher.Core.csproj`),
> 官方 host 侧的 JSON 序列化选项是 `PropertyNameCaseInsensitive = true` + `PropertyNamingPolicy = CamelCase`
> (见 `JsonRPCPluginBase.RequestSerializeOption`)——**所以宿主解析插件响应时大小写不敏感**。
> 这就是本运行时把插件写法的 `Title` 归一化成 `title` 也能被正确解析的原因。

### 1.3 关于「上游是否有现成的 V2 helper」的调研结论

**没有。** 这是本次调研最重要的结论,证据如下:

1. 上游 `Flow.Launcher` 仓库的 `dev` 分支(commit `947ba49f`)里**根本没有 `JsonRPC` 目录**,
   全仓库只有 C# 源码,没有任何 `.py` / `.js` helper。
   (`git ls-tree -r FETCH_HEAD` 的结果里匹配 `jsonrpc|\.py$|node|\.js$|\.ts$` 的只有
   `Flow.Launcher.Core/Plugin/JsonRPC*.cs`、`NodePluginV2.cs`、以及一个无关的
   `Flow.Launcher/Themes/ThemeBuilder/ThemeConvertor.py`。)
2. 官方发布的 helper(`flowlauncher` PyPI 包 / `Flow.Launcher.JsonRPC.Python` 仓库)
   实现的是**旧的一问一答协议**:`sys.argv[1]` 收 JSON,往 stdout `print` 一个 JSON,
   形如 `{"method": "query", "parameters": [""]}`。**不是** JSON-RPC 2.0,也不是常驻流。
3. 任务里提到的 `Flow-Launcher/python-plugin`、`Flow-Launcher/typescript-plugin`、
   `Flow-Launcher/node-plugin` 三个仓库**都不存在**(`git ls-remote` 全部失败)。
   `Flow-Launcher/Flow.Launcher.JsonRPC.Node` 也不存在。
4. `Flow.Launcher.JsonRPC.Python` 有一个 `rpc-standard` 分支,里面多了一个 `flowlauncher/RPC.py`
   (dataclass 容器),但仍然是旧协议(`from RPC import RPC` 甚至是坏的 import),没有参考价值。

所以:**V2 线路协议的上游 helper 不存在**,本目录是按上游公开的 API 约定 + 协议模型
**重新实现**的等价物,而不是拷贝。协议字段名以 §3 为准逐条对齐上游 C# 模型。

---

## 2. 怎么用

### 2.1 宿主侧(打包)

把 `runtime/python/` 整个目录放进程序目录下的 `JsonRPC/` 文件夹(即官方 Flow Launcher 的做法),
并像 `PythonPluginV2.cs` 那样设置环境变量:

```text
PYTHONPATH=<程序目录>\JsonRPC
PYTHONDONTWRITEBYTECODE=1
```

Python 插件仍按官方方式启动(`PythonPluginV2.cs`):`python -c "<code>"`,code 里把
插件根目录 / `lib` / `lib\win32` / `lib\win32\lib` / `plugin` 追加到 `sys.path`,
再用 `runpy.run_path(<插件 .py>, None, '__main__')` 执行。
插件再 `import python_plugin` 即可。

> 注意:官方是把插件自己的目录 **前置**到 `sys.path`(见官方文档的模板代码
> `sys.path = [str(plugindir / p) for p in paths] + sys.path`)。
> 如果插件自带了**旧版** `flowlauncher` 包(见 §7),它会盖过我们的 `flowlauncher.py` shim,
> 于是该插件仍然说旧协议、无法与 V2 宿主对话。这类插件需要作者升级或由宿主特判。

### 2.2 插件侧

```python
from python_plugin import FlowLauncher          # 或: from flowlauncher import FlowLauncher

class MyPlugin(FlowLauncher):
    def initialize(self, context):              # 宿主启动后调用一次,适合做初始化
        self.meta = context["currentPluginMetadata"]

    def query(self, query):                     # query 就是查询文本(见 §5)
        return [
            {
                "Title": "hello",               # 原版写法(PascalCase)照样能用
                "SubTitle": "world",
                "IcoPath": "Images/app.png",
                "JsonRPCAction": {"method": "open_url", "parameters": ["https://example.org"]},
            }
        ]

    def open_url(self, url):                    # 用户选中结果时,宿主回调本方法
        import webbrowser
        webbrowser.open(url)

if __name__ == "__main__":
    MyPlugin()
```

构造 `Plugin` 实例即开始服务宿主;宿主随后用 `initialize` / `query` / `context_menu` /
`reload_data` / `close` 驱动插件。

### 2.3 端到端自检

```powershell
py runtime\python\selftest.py     # 退出码 0 = PASS, 1 = FAIL
```

脚本用 `subprocess` 拉起 `selftest_plugin.py`,按行分隔发 `initialize` + `query`(+ `context_menu`
+ `close`)四个 JSON-RPC 请求,并校验收到的结果。实际运行输出见 §8。

---

## 3. 协议摘要(与 Rust 宿主严格一致)

### 3.1 帧格式

Python 插件 = **按行分隔**(`NewLineDelimited`)。一条 JSON 消息占一行,以 `\n` 结尾。

```jsonc
// 宿主 -> 插件
{"jsonrpc":"2.0","id":1,"method":"initialize","params":[{"currentPluginMetadata":{...},"api":{...}}]}
{"jsonrpc":"2.0","id":2,"method":"query","params":[<Query>, <Settings>]}
{"jsonrpc":"2.0","id":3,"method":"context_menu","params":[<contextData>]}
{"jsonrpc":"2.0","id":4,"method":"reload_data","params":[<PluginInitContext>]}
{"jsonrpc":"2.0","id":5,"method":"close","params":[]}

// 插件 -> 宿主(响应)
{"jsonrpc":"2.0","id":2,"result":{"id":2,"result":[<Result>,...],"settingsChange":null,"debugMessage":""}}

// 插件 -> 宿主(反向调用)
{"jsonrpc":"2.0","id":1,"method":"UpdateResults","params":["<trimmedQuery>", {"id":0,"result":[...],"settingsChange":null,"debugMessage":""}]}

// 错误
{"jsonrpc":"2.0","id":2,"error":{"code":-32603,"message":"...","data":null}}
```

* 插件回 `query` 的 `result` 形状 = `JsonRPCQueryResponseModel`
  (`id` / `result` / `settingsChange` / `debugMessage`),与 `JsonPRCModel.cs` 一致。
* `initialize` / `reload_data` / `close` 返回 `{}`(void 方法)。
* 插件自身的动作方法(如 `open_url`)被调用后返回 `{"hide":true}`
  (`JsonRPCExecuteResponse(bool Hide = true)`)。
* 非 ASCII 一律转义成 `\uXXXX`,保证 stdout 是纯 ASCII,不受控制台代码页影响。

### 3.2 宿主调用的方法

| 方法 | params | 插件侧回调 |
|---|---|---|
| `initialize` | `[<PluginInitContext>]` | `Plugin.initialize(context)` |
| `query` | `[<Query>, <Settings>]` | `Plugin.query(query)` |
| `context_menu` | `[<contextData>]` | `Plugin.context_menu(data)` |
| `reload_data` | `[<PluginInitContext>]` | `Plugin.reload_data(context)` |
| `close` | `[]` | `Plugin.close()` |
| *(其它名字)* | `[...]` | 插件实例上的同名方法(官方 `JsonRPCAction` 机制) |

### 3.3 插件可反向调用的宿主方法

`JsonRPCPublicAPI.cs` 全量清单(本运行时全部封装,见 §6):
`ChangeQuery` `RestartApp` `ShellRun` `CopyToClipboard` `SaveAppAllSettings` `SavePluginSettings`
`ReloadAllPluginDataAsync` `CheckForNewUpdate` `ShowMsgError` `ShowMainWindow` `HideMainWindow`
`IsMainWindowVisible` `ShowMsg` `OpenSettingDialog` `OpenPluginSettingsWindow` `GetTranslation`
`GetAllPlugins` `FuzzySearch` `HttpGetStringAsync` `HttpGetStreamAsync` `HttpDownloadAsync`
`AddActionKeyword` `RemoveActionKeyword` `ActionKeywordAssigned` `LogDebug` `LogInfo` `LogWarn`
`LogError` `OpenDirectory` `OpenUrl` `OpenAppUri` `BackToQueryResults` `StartLoadingBar`
`StopLoadingBar` `SavePluginCaches`,外加 `JsonRPCPluginV2.cs` 里注册的 `UpdateResults`。

### 3.4 Result 字段(camelCase)

`title` `subTitle` `titleToolTip` `subTitleToolTip` `icoPath` `badgeIcoPath` `glyph`
`actionKeywordAssigned` `copyText` `autoCompleteText` `score` `titleHighlightData` `contextData`
`pluginID` `progressBar` `progressBarColor` `preview` `previewVisibility` `addSelectedCount`
`recordKey` `showBadge` `querySuggestionText` `roundedIcon`
外加 `jsonRPCAction`(`{"id","method","parameters","settings","dontHideAfterAction"}`)与 `settingsChange`。

* `preview` 子对象:`previewImagePath` `isMedia` `description` `filePath` `contentType`
  (`"imageWithText"` / `"markdown"`)。
* `previewVisibility`:`"optional"` / `"never"` / `"always"`。

### 3.5 Query 字段

`originalQuery` `trimmedQuery` `isReQuery` `isHomeQuery` `search` `searchTerms` `actionKeyword`。

### 3.6 initialize 的 context

`{"currentPluginMetadata": {...}, "api": {...}}`;
`currentPluginMetadata` 至少含 `id` `name` `author` `version` `language` `description`
`website` `actionKeyword` `actionKeywords` `icoPath` `pluginDirectory`
`pluginSettingsDirectoryPath` `pluginCacheDirectoryPath`。

---

## 4. 原版 `Plugin` 基类的完整 API 清单

官方基类叫 **`FlowLauncher`**(见 `flowlauncher/FlowLauncher.py`)。
本运行时把它原样保留为 `FlowLauncher`,并额外提供别名 `Plugin`(两者是同一个类)。

| 成员 | 签名 | 作用 |
|---|---|---|
| `FlowLauncher.__init__` | `(self)` | 官方原版是「读 argv[1] → 调一次方法 → 打印结果」。**本运行时改为**:建立 JSON-RPC 传输、绑定宿主 API、开始服务宿主。构造完即返回,不阻塞。 |
| `FlowLauncher.query` | `(self, param: str = '') -> list` | 官方原版:子类需要覆盖的方法,返回结果列表。**本运行时**:宿主发来 `query` 时被调用,入参是 `Query` 对象(它 **就是** 字符串,见 §5),返回值必须是 list of dict / `Result`。 |
| `FlowLauncher.context_menu` | `(self, data) -> list` | 官方原版:结果右键菜单的可选项。**本运行时**:宿主发来 `context_menu` 时被调用,`data` 是被选中结果的 `contextData`。 |
| `FlowLauncher.debug` | `(self, msg: str)` | 官方原版:设置 `self.debugMessage`。**本运行时**:同样的字段名;宿主收到 query 响应后,若 `debugMessage` 非空会用 `ShowMsg` 弹出来(`JsonRPCPluginBase.ParseResults`)。 |
| `FlowLauncher.initialize` | `(self, context: dict)` | **新增(V2 专有)**:宿主发来 `initialize` 时被调用,入参是 `PluginInitContext`。官方 V1 没有对应物;插件的初始化逻辑应该放这里。 |
| `FlowLauncher.reload_data` | `(self, context: dict)` | **新增(V2 专有)**:宿主发来 `reload_data` 时被调用。 |
| `FlowLauncher.close` | `(self)` | **新增(V2 专有)**:宿主发来 `close` 时被调用,之后进程结束。 |
| `FlowLauncher.api` | property → `FlowLauncherAPI` | **新增**:当前插件实例的宿主 API。 |
| `FlowLauncher.stop()` | `(self)` | **新增**:主动结束服务。 |
| `FlowLauncher.wait_closed(timeout=None)` | `(self, timeout=None) -> bool` | **新增**:阻塞直到宿主断开。 |
| `FlowLauncher.exit_process_when_closed` | 类属性,默认 `True` | **新增**:宿主关闭连接后是否结束进程(见 §7 已知限制)。 |
| `FlowLauncher.debugMessage` | 实例属性 | 官方原版同名,`debug()` 写入它。 |

实例上还有:`self.context`(`PluginInitContext` 原文)、`self.current_plugin_metadata`、
`self.settings`(本次 query 带来的 settings)、`self.settings_change`(写进响应的 `settingsChange`)。

**官方 `FlowLauncherAPI` 的类方法**(原版是 `@classmethod`,直接 print 到 stdout):
`change_query(query, requery=False)`、`shell_run(cmd)`、`close_app()`、`hide_app()`、`show_app()`、
`show_msg(title, sub_title, ico_path="")`、`open_setting_dialog()`、`start_loadingbar()`、
`stop_loadingbar()`、`reload_plugins()`。本运行时**全部保留同名同参**,只是改为走 JSON-RPC。
详见 §6。

---

## 5. Result / Query 在 Python 侧怎么构造

### 5.1 Result —— 官方原版:**没有 Result 类,就是普通 dict**

官方 `FlowLauncher.py` 里 `query()` 的返回值直接被 `json.dumps` 出去,官方示例
(`Flow.Launcher.Plugin.HelloWorldPython/main.py`)就是一堆 dict:

```python
def query(self, query):
    return [
        {
            "Title": "Hello World, this is where title goes. {}".format(...),
            "SubTitle": "This is where your subtitle goes, press enter to open Flow's url",
            "IcoPath": "Images/app.png",
            "JsonRPCAction": {
                "method": "open_url",
                "parameters": ["https://github.com/Flow-Launcher/Flow.Launcher"]
            }
        }
    ]
```

本运行时**完全接受这种写法**,并把键名归一化成协议要求的 camelCase
(`Title`→`title`、`SubTitle`→`subTitle`、`IcoPath`→`icoPath`、`JsonRPCAction`→`jsonRPCAction`、
`ContextData`→`contextData`、`PluginID`→`pluginID` ……)。
归一化只作用于**已知的结果字段**和 `preview` / `jsonRPCAction` 两个子对象,
`contextData` 里用户自己的键不会被改动。

另外提供等价的 dataclass 写法(**函数名 = `Result`**,参数用 snake_case):

```python
from python_plugin import Result

Result(
    title="hello",
    sub_title="world",
    ico_path="Images/app.png",
    score=0,
    preview=None,                    # 或 Preview(preview_image_path=..., content_type="markdown")
    preview_visibility="optional",
    context_data={"foo": "bar"},
    action={"method": "open_url", "parameters": ["https://example.org"]},
)
```

`Result` 的字段(snake_case → JSON 名)与 `Result.cs` 一一对应:
`title`→`title`、`sub_title`→`subTitle`、`title_tooltip`→`titleToolTip`、
`sub_title_tooltip`→`subTitleToolTip`、`ico_path`→`icoPath`、`badge_ico_path`→`badgeIcoPath`、
`glyph`→`glyph`、`action_keyword_assigned`→`actionKeywordAssigned`、`copy_text`→`copyText`、
`auto_complete_text`→`autoCompleteText`、`score`→`score`、
`title_highlight_data`→`titleHighlightData`、`context_data`→`contextData`、`plugin_id`→`pluginID`,
`progress_bar`→`progressBar`、`progress_bar_color`→`progressBarColor`、`preview`→`preview`、
`preview_visibility`→`previewVisibility`、`add_selected_count`→`addSelectedCount`、
`record_key`→`recordKey`、`show_badge`→`showBadge`、`query_suggestion_text`→`querySuggestionText`、
`rounded_icon`→`roundedIcon`、`action`→`jsonRPCAction`、`settings_change`→`settingsChange`。
`Result.to_dict()` 负责序列化,`Result.from_dict()` 负责反序列化(任意大小写)。

### 5.2 Query —— 官方原版:**就是普通字符串**

官方 V1 的 `query()` 收到的是一个 `str`(Wox 协议传的是 `query.RawQuery`,即带 action keyword 的
trimmed query)。V2 协议传的是完整的 Query 对象。

本运行时用一个 **`Query(str)`** 把两者统一:

* `Query` **是** `str` 本身,值 = `trimmedQuery`(与官方 V1 的 `RawQuery` 语义一致);
  所以 `str(query)`、`query + "x"`、`query.split()`、`query.upper()` 都正常。
* V2 的完整信息以属性 / 下标形式提供:
  `query.search`、`query.trimmedQuery`、`query.originalQuery`、`query.isReQuery`、
  `query.isHomeQuery`、`query.searchTerms`、`query.actionKeyword`,
  以及 snake_case 别名 `query.trimmed_query`、`query.search_terms`、`query.action_keyword` …;
  也可以 `query["search"]` / `query.get("searchTerms")`。
* 构造函数:`Query(data)`,`data` 可以是宿主发来的 dict,也可以直接是字符串。

---

## 6. 宿主 API(`FlowLauncherAPI`)

位于 `runtime/python/flowlauncher_api.py`。类名、方法名、参数顺序与官方原版一致,
两种调用方式都支持:

```python
from flowlauncher_api import FlowLauncherAPI

FlowLauncherAPI.show_msg("标题", "副标题")     # 类级用法(官方原版的写法)
self.api.show_msg("标题", "副标题")            # 实例级用法(推荐,多插件时更安全)
```

| Python 方法 | 宿主方法 | 说明 |
|---|---|---|
| `change_query(query, requery=False)` | `ChangeQuery` | 官方原版同名 |
| `shell_run(cmd, filename="cmd.exe")` | `ShellRun` | 官方原版同名 |
| `close_app()` | `CloseApp` | 官方原版同名。**V2 宿主没有这个方法**,调用会收到 "method not found" |
| `hide_app()` | `HideMainWindow` | 官方原版 `hide_app` |
| `show_app()` | `ShowMainWindow` | 官方原版 `show_app` |
| `show_msg(title, sub_title="", ico_path="")` | `ShowMsg` | 官方原版同名 |
| `open_setting_dialog()` | `OpenSettingDialog` | 官方原版同名 |
| `start_loadingbar()` / `stop_loadingbar()` | `StartLoadingBar` / `StopLoadingBar` | 官方原版同名 |
| `reload_plugins()` | `ReloadAllPluginDataAsync` | 官方原版 `reload_plugins` |
| `update_results(trimmed_query, results, settings_change=None, debug_message="")` | `UpdateResults` | 边跑边推结果;params = `[trimmedQuery, response]` |
| `copy_to_clipboard(text, direct_copy=False, show_default_notification=True)` | `CopyToClipboard` | |
| `open_url(url, in_private=None)` | `OpenUrl` | |
| `open_directory(directory_path, file_name_or_file_path=None)` | `OpenDirectory` | |
| `open_app_uri(app_uri)` | `OpenAppUri` | |
| `fuzzy_search(query, string_to_compare)` | `FuzzySearch` | 有返回值 |
| `get_translation(key)` | `GetTranslation` | 有返回值 |
| `get_all_plugins()` | `GetAllPlugins` | 有返回值 |
| `add_action_keyword(plugin_id, kw)` / `remove_action_keyword(plugin_id, kw)` | `AddActionKeyword` / `RemoveActionKeyword` | |
| `action_keyword_assigned(kw)` | `ActionKeywordAssigned` | 有返回值 |
| `log_info / log_error / log_warn / log_debug(class_name, message, method_name="")` | `LogInfo` / `LogError` / `LogWarn` / `LogDebug` | |
| `show_msg_error(title, sub_title="")` | `ShowMsgError` | |
| `show_main_window()` / `hide_main_window()` / `is_main_window_visible()` | `ShowMainWindow` / `HideMainWindow` / `IsMainWindowVisible` | |
| `back_to_query_results()` | `BackToQueryResults` | |
| `start_loading_bar()` / `stop_loading_bar()` | `StartLoadingBar` / `StopLoadingBar` | |
| `save_app_all_settings()` / `save_plugin_settings()` / `save_plugin_caches()` | 同名 | |
| `restart_app()` / `check_for_new_update()` | 同名 | |
| `open_plugin_settings_window(plugin_id)` | 同名 | 有返回值 |
| `http_get_string_async(url)` / `http_get_stream_async(url)` / `http_download_async(url, file_path)` | 同名 | |
| `call(method, *params, timeout=30.0)` | 任意 | 兜底:直接调宿主任意方法 |

---

## 7. Node helper 的结论:**暂缺**

**没有找到可靠的上游实现,因此本目录不提供 `runtime/node/`。**

调研过程与证据:

1. `Flow-Launcher/node-plugin`、`Flow-Launcher/typescript-plugin`、`Flow-Launcher/Flow.Launcher.JsonRPC.Node`
   三个仓库 `git ls-remote` 全部失败 —— **不存在**。
2. 上游 `Flow.Launcher` 仓库的 `dev` 分支里**没有任何 `.js` / `.ts` 的运行时 helper**
   (`git ls-tree -r FETCH_HEAD` 只匹配到 `NodePluginV2.cs` 这个宿主侧文件)。
3. 官方发布的 `flowlauncher` PyPI 包只覆盖 Python,没有 Node 版。

因此:

* **社区 Node 插件需要自行实现协议**(或者我们后续自己补一个)。
* Node 侧的线路协议差异已经查清,写在 `NodePluginV2.cs` 里:
  Node 插件用 **`MessageHandlerType.HeaderDelimited`**,即
  `Content-Length: <字节数>\r\n\r\n<JSON>` 分帧(与 Python 的按行分隔不同);
  启动方式是 `node <插件 js 文件>`。
  Node 插件若要对接本宿主,必须自己实现这个分帧 + §3 的 JSON-RPC 2.0 帧格式。

---

## 8. 已知限制 / 取舍

* **进程退出**:读 stdin 的线程是 daemon 线程且会阻塞在 `readline()` 上,
  若让解释器带着它收尾,CPython 会在关闭 stdin 缓冲区时 abort
  (`could not acquire lock for <_io.BufferedReader name='<stdin>'> at interpreter shutdown`)。
  因此宿主发来 `close`(或关掉管道)后,运行时用 `os._exit(0)` 结束进程。
  需要在进程内使用本运行时(如单元测试)时,把 `Plugin.exit_process_when_closed` 设为 `False`。
* **线程模型**:一个读线程 + 一个派发线程 + 一个保活线程。
  请求按到达顺序串行处理,插件在 `query()` 里同步调用宿主 API 不会死锁
  (响应由读线程负责送达)。
* **子类的 `__init__`**:推荐把初始化逻辑写在 `initialize(context)` 里。
  如果子类在 `super().__init__()` 之后写初始化代码,运行时会等这段代码跑完
  才开始处理宿主的请求(已验证),所以两种顺序都安全。
* **只依赖标准库**:`json` `sys` `threading` `queue` `dataclasses` `functools` `os` `time` `traceback`。
* **`stdout` 是协议通道**:插件里任何 `print()` 都会破坏协议,必须改用 `print(..., file=sys.stderr)`。

---

## 9. 文件清单

| 文件 | 行数 | 作用 |
|---|---:|---|
| `python/python_plugin.py` | 485 | 主 helper:`Plugin` / `FlowLauncher` 基类、`Result`、`Preview`、`Query`、结果归一化、请求分发 |
| `python/jsonrpc_transport.py` | 320 | JSON-RPC 2.0 按行分隔的收发实现(标准库) |
| `python/flowlauncher_api.py` | 261 | `FlowLauncherAPI`:宿主 API 封装 |
| `python/flowlauncher.py` | 39 | 兼容 shim:`from flowlauncher import FlowLauncher` |
| `python/selftest.py` | 311 | 端到端自检脚本(PASS/FAIL + 退出码) |
| `python/selftest_plugin.py` | 43 | 最小示例插件 |
| `README.md` | — | 本文件 |

## 10. 许可

本目录的代码是按上游公开的 API 约定**重新实现**的(见 §1.3:上游没有 V2 helper),
其中 API 名称与文档结构参照了 MIT 许可的官方 helper 库
(https://github.com/Flow-Launcher/Flow.Launcher.JsonRPC.Python,MIT)。
协议字段定义参照了同样 MIT 许可的 Flow.Launcher 源码
(https://github.com/Flow-Launcher/Flow.Launcher,MIT)。
