#!/usr/bin/env python3
"""Legacy 2.0 personal migration tool; not a distribution installer.

Does not install Docker, change keys, create a tunnel, or touch other containers.
"""
import json, os, pathlib, plistlib, re, shutil, subprocess, sys, time
from datetime import datetime

project=pathlib.Path(__file__).resolve().parent.parent
home=pathlib.Path.home()
runtime=home/'.local/share/strigov-local-files-mcp'
agents=home/'Library/LaunchAgents'
logs=home/'Library/Logs/strigov-local-files-tunnel'
label='com.strigov.openai-tunnel.local-files'
tray_label='com.strigov.local-files.tray'
domain=f'gui/{os.getuid()}'
profile=home/'.config/tunnel-client/strigov-local-files-background.yaml'
source_bundle=project/'dist/Local Files.app'
app=home/'Applications/Local Files.app'
tunnel=shutil.which('tunnel-client')
binary=runtime/'local-files'

def run(args, check=True):
    return subprocess.run([str(a) for a in args],check=check,capture_output=True,text=True)
def write(path,text,mode=0o700):
    path.write_text(text);path.chmod(mode)
def quote(text):
    return "'"+str(text).replace("'","'\"'\"'")+"'"

if sys.platform!='darwin':sys.exit('Use the Windows installer on Windows.')
info=source_bundle/'Contents/Info.plist'
if info.is_file():
    with info.open('rb') as source:
        if plistlib.load(source).get('CFBundleIdentifier') == 'app.localfiles.desktop':
            sys.exit('Local Files 2.1 uses the standalone Install Local Files.app in the distribution DMG. This legacy personal migration script must not install it.')
if not source_bundle.is_dir() or not tunnel or not profile.is_file():
    sys.exit('Build the bundle and configure the existing tunnel first.')
if not pathlib.Path('/usr/bin/sandbox-exec').is_file():
    sys.exit('The required macOS worker sandbox is unavailable; installation stopped.')
# Read only the tunnel identity; runtime credentials remain in the existing Keychain item.
match=re.search(r'^\s*tunnel_id:\s*[\"\']?(tunnel_[a-z0-9]+)',profile.read_text(),re.M)
if not match:sys.exit('Existing tunnel identity was not found.')
run(['/usr/bin/security','find-generic-password','-a','strigov-local-files','-s','com.strigov.openai-tunnel.local-files.runtime-key'])
for directory in [runtime,agents,logs,app.parent]:directory.mkdir(parents=True,exist_ok=True)
backup=runtime/'backups'/datetime.now().strftime('%Y%m%d-%H%M%S')
backup.mkdir(parents=True,mode=0o700)
targets=[profile,agents/f'{label}.plist',agents/f'{tray_label}.plist',runtime/'run-background-service.sh',runtime/'run-rust-server.sh',runtime/'readonly.sb',binary]
manifest=[]
for index,path in enumerate(targets):
    name=f'{index}-{path.name}'
    if path.exists():shutil.copy2(path,backup/name)
    manifest.append({'path':str(path),'backup':name,'existed':path.exists()})
(backup/'manifest.json').write_text(json.dumps(manifest,indent=2))
was_running='state = running' in run(['/bin/launchctl','print',f'{domain}/{label}'],False).stdout
tray_was_running='state = running' in run(['/bin/launchctl','print',f'{domain}/{tray_label}'],False).stdout
try:
    run(['/bin/launchctl','bootout',f'{domain}/{tray_label}'],False)
    run(['/bin/launchctl','bootout',f'{domain}/{label}'],False)
    if app.exists():shutil.move(str(app),str(backup/'Local Files.app'))
    shutil.copytree(source_bundle,app)
    shutil.copy2(app/'Contents/MacOS/local-files',binary)
    run([binary,'init'])
    # The worker cannot write files, use network sockets, or fork. Folder scoping is
    # separately enforced through cap-std directory handles and blocked-path rules.
    write(runtime/'readonly.sb','(version 1)\n(allow default)\n(deny file-write*)\n(deny network*)\n(deny process-fork)\n',0o600)
    write(runtime/'run-rust-server.sh',f'''#!/bin/sh
set -eu
unset CONTROL_PLANE_API_KEY OPENAI_API_KEY
exec /usr/bin/sandbox-exec -f {quote(runtime/'readonly.sb')} {quote(binary)} serve --config {quote(runtime/'folders.json')}
''')
    write(runtime/'run-background-service.sh',f'''#!/bin/sh
set -eu
export PATH='/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin'
CONTROL_PLANE_API_KEY=$(/usr/bin/security find-generic-password -a 'strigov-local-files' -s 'com.strigov.openai-tunnel.local-files.runtime-key' -w)
export CONTROL_PLANE_API_KEY
exec /usr/bin/caffeinate -i {quote(tunnel)} run --profile strigov-local-files-background
''')
    contents,count=re.subn(r'(?m)^(\s+command:) .*$',lambda m:m.group(1)+' '+json.dumps(str(runtime/'run-rust-server.sh')),profile.read_text())
    if count!=1:raise RuntimeError('Expected exactly one MCP command in the existing profile')
    temporary=profile.with_suffix('.yaml.new')
    write(temporary,contents,0o600)
    os.replace(temporary,profile)
    daemon={'Label':label,'ProgramArguments':[str(runtime/'run-background-service.sh')],'WorkingDirectory':str(runtime),
        'EnvironmentVariables':{'HOME':str(home),'PATH':'/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin'},
        'RunAtLoad':True,'KeepAlive':True,'ThrottleInterval':15,'ProcessType':'Background','LimitLoadToSessionType':'Aqua',
        'StandardOutPath':str(logs/'stdout.log'),'StandardErrorPath':str(logs/'stderr.log')}
    tray={'Label':tray_label,'ProgramArguments':[str(app/'Contents/MacOS/local-files'),'tray'],'RunAtLoad':True,
        'ProcessType':'Interactive','LimitLoadToSessionType':'Aqua','StandardOutPath':str(logs/'tray.stdout.log'),'StandardErrorPath':str(logs/'tray.stderr.log')}
    for name,data in [(label,daemon),(tray_label,tray)]:
        target=agents/f'{name}.plist'
        with target.open('wb') as out:plistlib.dump(data,out)
        target.chmod(0o600)
        run(['/bin/launchctl','enable',f'{domain}/{name}'])
        run(['/bin/launchctl','bootstrap',domain,target])
    verified=False
    for _ in range(25):
        health=run([tunnel,'health','--port','8080','--json','--require-control-plane-poll'],False)
        if health.returncode==0:verified=True;break
        time.sleep(2)
    if not verified:raise RuntimeError('Tunnel readiness was not confirmed; restoring previous installation')
    print(json.dumps({'installed':str(app),'backup':str(backup),'tunnel_ready':True},ensure_ascii=False))
except Exception as e:
    run(['/bin/launchctl','bootout',f'{domain}/{tray_label}'],False)
    run(['/bin/launchctl','bootout',f'{domain}/{label}'],False)
    for item in manifest:
        target=pathlib.Path(item['path'])
        if item['existed']:shutil.copy2(backup/item['backup'],target)
        elif target.exists():target.unlink()
    if app.exists():shutil.rmtree(app)
    if (backup/'Local Files.app').exists():shutil.move(str(backup/'Local Files.app'),str(app))
    if was_running:run(['/bin/launchctl','bootstrap',domain,agents/f'{label}.plist'],False)
    if tray_was_running:run(['/bin/launchctl','bootstrap',domain,agents/f'{tray_label}.plist'],False)
    print(f'Installation failed and was rolled back: {e}',file=sys.stderr)
    print(f'Backup: {backup}',file=sys.stderr)
    sys.exit(1)
