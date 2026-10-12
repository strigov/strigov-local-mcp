#!/usr/bin/env python3
"""Restore a specific installation backup, preserving the current files in a new backup."""
import json, pathlib, shutil, subprocess, sys, os
from datetime import datetime

if len(sys.argv)!=2:sys.exit('Usage: python3 scripts/restore-native-backup.py BACKUP_DIRECTORY')
backup=pathlib.Path(sys.argv[1]).resolve()
home=pathlib.Path.home();runtime=home/'.local/share/strigov-local-files-mcp'
if backup.parent!=runtime/'backups':sys.exit('Select an installation backup under the runtime backups directory')
manifest=json.loads((backup/'manifest.json').read_text())
allowed={str(runtime/name) for name in ['run-background-service.sh','run-rust-server.sh','readonly.sb','local-files']}
allowed.update([str(home/'.config/tunnel-client/strigov-local-files-background.yaml'),str(home/'Library/LaunchAgents/com.strigov.openai-tunnel.local-files.plist'),str(home/'Library/LaunchAgents/com.strigov.local-files.tray.plist')])
for item in manifest:
    if item['path'] not in allowed or pathlib.Path(item['backup']).name!=item['backup']:sys.exit('Invalid backup manifest')
    if item['existed'] and not (backup/item['backup']).is_file():sys.exit('Incomplete backup')
current=runtime/'backups'/('before-restore-'+datetime.now().strftime('%Y%m%d-%H%M%S'));current.mkdir(mode=0o700)
current_manifest=[]
for item in manifest:
    p=pathlib.Path(item['path'])
    if p.exists():shutil.copy2(p,current/item['backup'])
    current_manifest.append({**item,'existed':p.exists()})
(current/'manifest.json').write_text(json.dumps(current_manifest,indent=2))
domain=f'gui/{os.getuid()}'
for label in ['com.strigov.local-files.tray','com.strigov.openai-tunnel.local-files']:
    subprocess.run(['/bin/launchctl','bootout',f'{domain}/{label}'],capture_output=True)
for item in manifest:
    p=pathlib.Path(item['path'])
    if item['existed']:shutil.copy2(backup/item['backup'],p)
    elif p.exists():p.unlink()
app=home/'Applications/Local Files.app'
if app.exists():shutil.move(str(app),str(current/'Local Files.app'))
if (backup/'Local Files.app').exists():shutil.copytree(backup/'Local Files.app',app)
print(f'Restored configuration from {backup}. Current files preserved in {current}.')
print('The restored service is stopped. Start it with its corresponding installer or launchctl bootstrap.')
