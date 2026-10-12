param(
    [Parameter(Mandatory=$true)][string]$TunnelId,
    [Parameter(Mandatory=$true)][string]$TunnelClient
)
$ErrorActionPreference = 'Stop'
if ($TunnelId -notmatch '^tunnel_[a-z0-9]+$') { throw 'Invalid tunnel ID' }
$ProjectDir = Split-Path -Parent $PSScriptRoot
$SourceExe = Join-Path $ProjectDir 'target\release\local-files.exe'
if (!(Test-Path $SourceExe)) { throw 'Build with cargo build --release --locked first.' }
$SourceVersion = (& $SourceExe --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $SourceVersion -ne 'Local Files 2.0.0') {
    throw 'This legacy installer supports only Local Files 2.0.0. The distributable Windows installer for 2.1 is not available yet.'
}
$TunnelClient = (Resolve-Path $TunnelClient).Path
$RuntimeDir = Join-Path $env:LOCALAPPDATA 'Local Files'
New-Item -ItemType Directory -Force $RuntimeDir | Out-Null
New-Item -ItemType Directory -Force (Join-Path $RuntimeDir 'logs') | Out-Null
$Exe = Join-Path $RuntimeDir 'local-files.exe'
Copy-Item $SourceExe $Exe -Force
& $Exe init
if ($LASTEXITCODE -ne 0) { throw 'Failed to initialize folder settings' }
# SecureString Export-Clixml uses Windows DPAPI, bound to this user and computer.
$SecretPath = Join-Path $RuntimeDir 'runtime-key.xml'
if (!(Test-Path $SecretPath)) {
    $Secret = Read-Host 'OpenAI tunnel runtime API key' -AsSecureString
    $Secret | Export-Clixml $SecretPath
}
$ProfileDir = Join-Path $RuntimeDir 'tunnel'
$ConfigPath = Join-Path $RuntimeDir 'folders.json'
& $TunnelClient init --sample sample_mcp_stdio_local --profile local-files-rust --profile-dir $ProfileDir --tunnel-id $TunnelId --mcp-command "`"$Exe`" serve --config `"$ConfigPath`""
if ($LASTEXITCODE -ne 0) { throw 'Profile already exists or could not be created; existing profile was preserved.' }
$Runner = Join-Path $RuntimeDir 'run-tunnel.ps1'
$EscapedClient = $TunnelClient.Replace("'", "''")
$EscapedDir = $RuntimeDir.Replace("'", "''")
@"
`$ErrorActionPreference = 'Stop'
`$root = '$EscapedDir'
`$secure = Import-Clixml (Join-Path `$root 'runtime-key.xml')
`$credential = New-Object System.Management.Automation.PSCredential('runtime', `$secure)
`$env:CONTROL_PLANE_API_KEY = `$credential.GetNetworkCredential().Password
try {
    & '$EscapedClient' run --profile local-files-rust --profile-dir (Join-Path `$root 'tunnel') *>> (Join-Path `$root 'logs\tunnel.log')
    exit `$LASTEXITCODE
} finally { Remove-Item Env:CONTROL_PLANE_API_KEY -ErrorAction SilentlyContinue }
"@ | Set-Content -Encoding UTF8 $Runner
$Action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -WindowStyle Hidden -File `"$Runner`""
$Trigger = New-ScheduledTaskTrigger -AtLogOn -User ([System.Security.Principal.WindowsIdentity]::GetCurrent().Name)
$Settings = New-ScheduledTaskSettingsSet -RestartCount 20 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName 'Local Files Tunnel' -Action $Action -Trigger $Trigger -Settings $Settings -Description 'Read-only Local Files MCP tunnel' | Out-Null
$Startup = [Environment]::GetFolderPath('Startup')
$Shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $Startup 'Local Files.lnk'))
$Shortcut.TargetPath = $Exe
$Shortcut.Arguments = 'tray'
$Shortcut.Save()
Start-ScheduledTask -TaskName 'Local Files Tunnel'
Start-Process $Exe -ArgumentList 'tray'
Write-Output "Installed: $RuntimeDir. Use the tray menu to choose folders."
