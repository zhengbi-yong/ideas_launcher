; Inno Setup script for IDEAS Launcher.
;
; The application is installed per user (no administrator rights needed), into
; %LOCALAPPDATA%\Programs\IdeasLauncher. Everything else — the shortcuts, the
; uninstaller, the optional start-with-Windows entry — is created by this
; script.

#define MyAppName "IDEAS Launcher"
#define MyAppVersion "1.0.0"
#define MyAppPublisher "IDEAS"
#define MyAppExeName "IdeasLauncher.exe"
#define MyAppUrl ""

; The application id.
;
; It is what tells Inno Setup that it is looking at an *upgrade* of the same
; program, so a verification build has to use a different one — otherwise
; testing the installer would uninstall the copy the user actually runs.
#ifndef MyAppId
  #define MyAppId "{{5C1A9B2E-7D43-4F86-9E0B-3A6C8D5E2F71}"
#endif
#ifndef MyOutputBase
  #define MyOutputBase "IdeasLauncher-" + MyAppVersion + "-setup"
#endif
#ifndef MyOutputDir
  #define MyOutputDir "..\dist"
#endif
; The Start Menu group. A verification build uses its own name, so that its
; uninstaller cannot remove the shortcuts of the real installation.
#ifndef MyGroupName
  #define MyGroupName MyAppName
#endif

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppComments=用 Rust 编写的 Windows 快速启动器
VersionInfoVersion={#MyAppVersion}
VersionInfoDescription={#MyAppName}
VersionInfoProductName={#MyAppName}
VersionInfoProductVersion={#MyAppVersion}
VersionInfoCompany={#MyAppPublisher}

; A per-user installation: no UAC prompt, and the uninstaller shows up in the
; user's own list of installed programs.
;
; The index service is a different matter: it runs as SYSTEM, and only an
; administrator may stop it. The script asks for that one moment of
; elevation when it is needed — see the [Code] section.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={localappdata}\Programs\IdeasLauncher
DefaultGroupName={#MyGroupName}
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
OutputBaseFilename={#MyOutputBase}
OutputDir={#MyOutputDir}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ShowLanguageDialog=no
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
; The files that are in use are dealt with in the [Code] section, where the
; wait is bounded and nothing is left to the user's guesswork. The dialog
; "these programs are using files that need to be updated — close them or
; restart later?" is exactly where an installation appears to hang, and the
; "restart later" answer leaves the new program half-installed.
CloseApplications=no
RestartApplications=no
; A log of what the installer did, in the user's temporary directory. It is
; the first thing to ask for when an installation does not go the way it
; should — without it there is nothing to look at.
SetupLogging=yes
SetupIconFile=..\crates\launcher-app\resources\app.ico

[Languages]
; The Chinese messages of Inno Setup. They ship with the program itself, so
; the installer does not depend on the network for them.
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"
Name: "autostart"; Description: "开机自动启动(注册表 Run 项)"; GroupDescription: "启动行为"
Name: "runnow"; Description: "安装完成后立即启动 IDEAS Launcher"; GroupDescription: "启动行为"

[Files]
Source: "..\dist\staging\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
; The service that holds the index of the whole machine. The launcher starts
; it with administrative rights when the user asks for it, so it has to sit
; next to the launcher.
Source: "..\dist\staging\ideas-launcher-service.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\staging\快速开始.txt"; DestDir: "{app}"; Flags: ignoreversion
; The runtime helpers for the external plugins. The launcher puts this
; directory on PYTHONPATH, so a plugin can simply `import python_plugin`.
Source: "..\dist\staging\runtime\*"; DestDir: "{app}\runtime"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\{#MyAppExeName}"
Name: "{group}\使用说明"; Filename: "{app}\快速开始.txt"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: """{app}\{#MyAppExeName}"""; Flags: uninsdeletevalue; Tasks: autostart

[UninstallDelete]
; Python may still have written byte code into the runtime directory, and the
; files list only knows about the files that were shipped.
Type: filesandordirs; Name: "{app}\runtime"
; The old programs that were moved aside to make room for the new ones.
; They are not part of the file list, so they have to be named here.
Type: files; Name: "{app}\*.old*"

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent; Tasks: runnow

[Code]
const
  ServiceName = 'IdeasLauncherService';
  ServiceExe = 'ideas-launcher-service.exe';
  LauncherExe = 'IdeasLauncher.exe';

var
  { 被挪开让位的旧文件。等旧进程撒手之后就可以扔掉。 }
  MovedAside: TStringList;
  { 这一次有没有托后台助手去换掉索引服务。 }
  HelperStarted: Boolean;

{ 文件名去掉扩展名。 }
function StripExtension(const Name: String): String;
var
  I: Integer;
  Dot: Integer;
begin
  Dot := 0;
  for I := 1 to Length(Name) do
    if Name[I] = '.' then
      Dot := I;
  if Dot > 0 then
    Result := Copy(Name, 1, Dot - 1)
  else
    Result := Name;
end;

{ 结束一个程序。
  窗口是自己启动的进程,普通权限就能结束它。
  服务是系统进程,只有管理员能结束它 —— 那里失败也不打紧:
  文件会先被挪开让位,安装照样走得下去,助手会去把服务真正停掉。 }
procedure EndProgram(const ImageName: String);
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM "' + ImageName + '"',
    '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

{ 把文件挪开,给新文件让出名字。

  正在运行的文件不能被写覆盖 —— 而写不进去的文件,就是安装停下来的地方。

  改名却可以:Windows 允许给正在运行的程序改名(只是不肯删它),
  旧程序从它手里那个文件接着跑,名字空出来给新程序。
  安装之所以不会卡在被占用的文件上,靠的就是这一手。 }
procedure MoveAside(const Name: String);
var
  Target: String;
  Number: Integer;
begin
  if not FileExists(Name) then
    Exit;
  for Number := 1 to 100 do
  begin
    Target := StripExtension(Name) + '.old-' + IntToStr(Number) + '.exe';
    { 上一次安装留下的旧件可能还在那儿。删得掉,名字就又空出来了。 }
    DeleteFile(Target);
    if (not FileExists(Target)) and RenameFile(Name, Target) then
    begin
      if MovedAside = nil then
        MovedAside := TStringList.Create;
      MovedAside.Add(Target);
      Exit;
    end;
  end;
end;

{ 让新文件写得进去。
  窗口是自己启动的进程,普通权限就能结束它。
  然后把两个程序挪开改名 —— 正在运行的文件也可以改名,名字一让开,
  新文件就写得进去,安装不会停在"文件被占用"上。

  服务不在这里停:它是系统进程,只有管理员能停。
  提权的事交给后台的助手去做,安装本身不等它。 }
procedure MakeRoom;
begin
  EndProgram(LauncherExe);
  MoveAside(ExpandConstant('{app}\') + ServiceExe);
  MoveAside(ExpandConstant('{app}\') + LauncherExe);
end;

{ 服务到底在不在?
  没装过的服务没什么可停、没什么可起,也不值得为此去问管理员的权利。
  `sc query` 找不到服务时说的是 1060,找得到(哪怕没在跑)说的是 0。 }
function ServiceExists: Boolean;
var
  ResultCode: Integer;
begin
  Result := True;
  if Exec(ExpandConstant('{sys}\sc.exe'), 'query "' + ServiceName + '"',
      '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    Result := (ResultCode <> 1060) and (ResultCode <> 1062);
end;

{ 助手脚本和标记文件待的地方。

  不用 Inno 那个 tmp 常量 —— 那是安装程序自己的临时子目录,
  安装一结束就被删掉,而助手要活到那之后。
  用用户自己的临时目录,那是稳的地方。 }
function ScratchFolder: String;
begin
  Result := ExpandConstant('{localappdata}') + '\Temp';
  CreateDir(Result);
end;

{ 写提权助手要用的两个脚本。

  助手做三件事:停掉旧服务、结束还在跑的旧进程、再把服务起回来 ——
  这一次是从新程序起的。

  它在起服务之前会等一个标记:新程序得先摆到位,标记就是"到位了"的信号。
  每一次等待都有上限,助手等不到就自己走掉,不会永远站在那儿。

  卸载的时候不用等:服务只要停下来,剩下的事就是清理。 }
procedure WriteHelperScripts(const Uninstalling: Boolean);
var
  Folder: String;
  App: String;
  Stage: String;
  Text: String;
begin
  { 助手要活到安装程序退出之后,所以脚本不能写在安装器自己的临时目录里 ——
    那个目录安装一结束就被删掉。 }
  Folder := ScratchFolder;
  App := ExpandConstant('{app}');
  Stage := Folder + '\ideas-copy-done.flag';

  Text :=
    '@echo off' + #13#10 +
    'setlocal' + #13#10 +
    'set "LOG=%TEMP%\ideas-upgrade.log"' + #13#10 +
    'set "APPDIR=' + App + '"' + #13#10 +
    'set "SVC=' + ServiceName + '"' + #13#10 +
    'set "STAGE=' + Stage + '"' + #13#10 +
    'set /a N=0' + #13#10 +
    'sc query "%SVC%" >>"%LOG%" 2>&1' + #13#10 +
    'if errorlevel 1 goto :done' + #13#10 +
    'sc stop "%SVC%" >>"%LOG%" 2>&1' + #13#10 +
    { 停要等它真的停下来。一个被 taskkill 强杀的服务进程来不及告诉
      服务控制管理器"我结束了",管理器就把它一直记在账上 —— 死进程
      的句柄留在管理器手里,一次升级留一个。轮询 sc query 直到
      STATE 变成 STOPPED(最多约 15 秒),taskkill 只留给卡死的服务。 }
    'set /a W=0' + #13#10 +
    ':waitstop' + #13#10 +
    'sc query "%SVC%" 2>nul | find "STOPPED" >nul' + #13#10 +
    'if not errorlevel 1 goto :stopped' + #13#10 +
    'set /a W+=1' + #13#10 +
    'if %W% GEQ 15 goto :kill' + #13#10 +
    'ping -n 2 127.0.0.1 >nul' + #13#10 +
    'goto :waitstop' + #13#10 +
    ':kill' + #13#10 +
    'taskkill /F /IM "' + ServiceExe + '" >>"%LOG%" 2>&1' + #13#10 +
    'ping -n 2 127.0.0.1 >nul' + #13#10 +
    ':stopped' + #13#10 +
    'rem 窗口不用在这里杀:安装程序自己早就关过旧窗口了,' + #13#10 +
    'rem 在这里杀会把刚装完启动的新窗口也杀掉。' + #13#10 +
    'for /f "delims=" %%f in (''dir /b /a "%APPDIR%\*.old*" 2^>nul'') do taskkill /F /IM "%%~nx" >>"%LOG%" 2>&1' + #13#10 +
    'ping -n 2 127.0.0.1 >nul' + #13#10;
  if not Uninstalling then
    Text := Text +
      ':wait' + #13#10 +
      'if exist "%STAGE%" goto :go' + #13#10 +
      'set /a N+=1' + #13#10 +
      'if %N% GEQ 100 goto :done' + #13#10 +
      'ping -n 3 127.0.0.1 >nul' + #13#10 +
      'goto :wait' + #13#10;
  Text := Text +
    ':go' + #13#10 +
    'del /q "%STAGE%" >>"%LOG%" 2>&1' + #13#10;
  if not Uninstalling then
    Text := Text +
      'sc start "%SVC%" >>"%LOG%" 2>&1' + #13#10 +
      'if not errorlevel 1 goto :clean' + #13#10 +
      'ping -n 3 127.0.0.1 >nul' + #13#10 +
      'sc start "%SVC%" >>"%LOG%" 2>&1' + #13#10 +
      'if not errorlevel 1 goto :clean' + #13#10 +
      '"%APPDIR%\' + ServiceExe + '" --install --profile "%USERPROFILE%" >>"%LOG%" 2>&1' + #13#10;
  { 挪开的旧文件最后再删:旧进程刚刚才撒手,早一点删是删不掉的。 }
  Text := Text +
    ':clean' + #13#10 +
    'del /q "%APPDIR%\*.old*" >nul 2>&1' + #13#10 +
    ':done' + #13#10 + 'endlocal' + #13#10;
  SaveStringToFile(Folder + '\ideas-elevated.cmd', Text, False);

  { 第二个脚本去问 Windows 要管理员的权利,再把活交给第一个脚本。
    它自己写一份日志:出问题时第一眼要看的,就是它走到了哪一步。

    提权的那一下子有一个坑:路径要交给另一个进程去执行,
    而 Start-Process 遇到带空格的参数会自己再套一层引号,
    套两层的路径就找不到了。所以路径不走参数,走环境变量 ——
    子进程继承环境,cmd 自己把它展开。

    脚本里只有 ASCII:Inno 写文件用的是系统代码页,
    非中文系统上中文字会被换成问号,虽然不影响运行,可是日志就没法看了。 }
  Text :=
    '$ErrorActionPreference = "SilentlyContinue"' + #13#10 +
    '$log = Join-Path $env:TEMP "ideas-elevate.log"' + #13#10 +
    'function Note($m) { Add-Content -Path $log -Value ("{0:HH:mm:ss} {1}" -f (Get-Date), $m) }' + #13#10 +
    '$id = [Security.Principal.WindowsIdentity]::GetCurrent()' + #13#10 +
    '$principal = New-Object Security.Principal.WindowsPrincipal $id' + #13#10 +
    '$admin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)' + #13#10 +
    'Note ("pid={0} admin={1}" -f $PID, $admin)' + #13#10 +
    'if ($admin) {' + #13#10 +
    '  $cmd = Join-Path $PSScriptRoot "ideas-elevated.cmd"' + #13#10 +
    '  Note ("running {0}" -f $cmd)' + #13#10 +
    '  & $cmd' + #13#10 +
    '  Note ("done, exit code {0}" -f $LASTEXITCODE)' + #13#10 +
    '  exit 0' + #13#10 +
    '}' + #13#10 +
    '$env:IDEAS_HELPER = (Join-Path $PSScriptRoot "ideas-elevated.cmd")' + #13#10 +
    '$shell = Join-Path $env:SystemRoot "System32\cmd.exe"' + #13#10 +
    'Note "asking Windows for administrator rights"' + #13#10 +
    'Start-Process -FilePath $shell -ArgumentList "/c", ''"%IDEAS_HELPER%"'' -Verb RunAs -WindowStyle Hidden' + #13#10 +
    'Note "asked"' + #13#10;
  SaveStringToFile(Folder + '\ideas-elevate.ps1', Text, False);
end;

{ 让助手在后台跑起来。

  这就是以前把安装卡住的那一步:向 Windows 要管理员权利用的是 `runas`,
  而 shell 一定要等这个回答 —— 等的就是那个要人点的对话框。
  所以这个问题要在一个别的进程里去问,安装程序自己走它的路。

  等回答的不是安装程序:那时候安装程序自己的活已经干完了。 }
procedure StartHelper;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}') + '\WindowsPowerShell\v1.0\powershell.exe',
    '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' +
      ScratchFolder + '\ideas-elevate.ps1"',
    '', SW_HIDE, ewNoWait, ResultCode);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  { 每一次安装都从干净状态开始:上一次留下的"文件已换好"标记要先拿走,
    否则助手会以为新程序早就摆到位了。 }
  DeleteFile(ScratchFolder + '\ideas-copy-done.flag');

  MakeRoom;

  { 服务是系统进程,只有管理员能停。助手去问那个权利,在后台问,
    安装不在原地等回答。 }
  HelperStarted := ServiceExists;
  if HelperStarted then
  begin
    WriteHelperScripts(False);
    StartHelper;
  end;

  { 空字符串的意思是"往下走"。 }
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  I: Integer;
  ServiceText: String;
begin
  if CurStep = ssPostInstall then
  begin
    { 新程序摆到位了 —— 助手可以把服务起回来了。 }
    SaveStringToFile(ScratchFolder + '\ideas-copy-done.flag', 'done' + #13#10, False);

    { 挪开的旧文件没有用了。能扔就扔;旧进程还占着就扔不掉,
      那就交给助手,它把旧进程结束之后会再来扔一次。 }
    if MovedAside <> nil then
    begin
      for I := 0 to MovedAside.Count - 1 do
        DeleteFile(MovedAside[I]);
      MovedAside.Free;
      MovedAside := nil;
    end;

    if HelperStarted then
      ServiceText :=
        'Windows 会问一次管理员权限 —— 点「是」,索引服务就换成新程序起来,' + #13#10 +
        '全盘索引由它管着。(不点也能用:旧服务接着跑,重启电脑后自动换成新版。)'
    else
      ServiceText :=
        '想让全盘索引由系统服务来管的话,在设置里点一下「安装索引服务」,' + #13#10 +
        'Windows 会问一次管理员权限 —— 点「是」就行。';

    if not WizardSilent then
      MsgBox('安装完成。' + #13#10 + #13#10 +
        ServiceText + #13#10 + #13#10 +
        '默认热键为 Alt + Space;若已被其它程序占用(例如 Flow Launcher),' + #13#10 +
        '会自动回退到 Ctrl + Space。实际生效的组合显示在窗口右下角。' + #13#10 + #13#10 +
        '数据目录:%APPDATA%\IdeasLauncher', mbInformation, MB_OK);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    { 窗口是自己启动的进程,普通权限就能结束它。 }
    EndProgram(LauncherExe);

    { 正在跑的服务文件删不掉,改名之后就删得掉了 ——
      卸载器要删的是原来的名字。 }
    MoveAside(ExpandConstant('{app}\') + ServiceExe);
    MoveAside(ExpandConstant('{app}\') + LauncherExe);

    { 服务是系统进程,只有管理员能停。助手去停,顺手把旧文件清掉。
      它在后台跑,卸载不等它。 }
    if ServiceExists then
    begin
      WriteHelperScripts(True);
      StartHelper;
    end;
  end;
end;
