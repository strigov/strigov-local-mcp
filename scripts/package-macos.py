#!/usr/bin/env python3
"""Build fresh, credential-free bundles from an explicit file allowlist."""
import json, os, pathlib, platform, plistlib, re, shutil, subprocess, tempfile

project=pathlib.Path(__file__).resolve().parent.parent
version=re.search(r'^version = "([^"]+)"', (project/'Cargo.toml').read_text(), re.M)[1]
arch={'arm64':'arm64','x86_64':'amd64'}[platform.machine()]
lock=json.loads((project/'scripts/tunnel-client.lock.json').read_text())
vendor=project/'artifacts/vendor'/lock['version']/('darwin-'+arch)
runtime=[p for p in (vendor/'unpacked').rglob('*') if p.is_file() and p.name in ['tunnel-client','tunnel-client-runtime']]
if len(runtime)!=1: raise SystemExit('Fetch the official runtime first')
dist=project/'dist'
dist.mkdir(exist_ok=True)
identity=os.environ.get('LOCAL_FILES_SIGNING_IDENTITY','-')

def bundle(root,name,identifier,executable,source):
    app=root/(name+'.app')
    macos=app/'Contents/MacOS';resources=app/'Contents/Resources'
    macos.mkdir(parents=True);resources.mkdir()
    shutil.copy2(source,macos/executable)
    (macos/executable).chmod(0o755)
    info={'CFBundleIdentifier':identifier,'CFBundleName':name,'CFBundleDisplayName':name,
        'CFBundleExecutable':executable,'CFBundlePackageType':'APPL',
        'CFBundleShortVersionString':version,'CFBundleVersion':version,
        'LSMinimumSystemVersion':'13.0','LSUIElement':True,'NSHighResolutionCapable':True,
        'NSDocumentsFolderUsageDescription':'Чтение только явно выбранных вами папок через MCP.'}
    with (app/'Contents/Info.plist').open('wb') as out: plistlib.dump(info,out)
    return app,resources

def sign(path):
    args=['/usr/bin/codesign','--force','--sign',identity]
    if identity!='-': args+=['--options','runtime','--timestamp']
    subprocess.run(args+[str(path)],check=True)

with tempfile.TemporaryDirectory(prefix='local-files-package-',dir=dist) as temp:
    stage=pathlib.Path(temp)
    app,resources=bundle(stage,'Strigov Local MCP','app.localfiles.desktop','local-files',project/'target/release/local-files')
    shutil.copy2(runtime[0],resources/'tunnel-client');(resources/'tunnel-client').chmod(0o755)
    for name in ['LICENSE','NOTICE']:
        shutil.copy2(vendor/'unpacked'/name,resources/('tunnel-client-'+name+'.txt'))
    for asset in lock['platforms']['darwin-'+arch]:
        if not asset['name'].endswith('.zip'): shutil.copy2(vendor/asset['name'],resources/asset['name'])
    shutil.copy2(project/'docs/distribution-readme.txt',resources/'Read Me.txt')
    metadata=json.loads(subprocess.check_output(['cargo','metadata','--locked','--format-version','1'],cwd=project))
    notices=[]
    for package in metadata['packages']:
        if package['source'] is None: continue
        root=pathlib.Path(package['manifest_path']).parent
        texts=[]
        for file in sorted(root.iterdir()):
            if file.is_file() and (file.name.upper().startswith('LICENSE') or file.name.upper().startswith('COPYING')):
                texts.append(file.read_text(errors='replace'))
        notices.append(package['name']+' '+package['version']+' ('+str(package.get('license'))+')\n'+'\n'.join(texts))
    (resources/'Third Party Notices.txt').write_text('\n\n-----\n\n'.join(notices))
    sign(resources/'tunnel-client');sign(app)
    installer,setup_resources=bundle(stage,'Install Strigov Local MCP','app.localfiles.installer','local-files-installer',project/'target/release/local-files-installer')
    shutil.copytree(app,setup_resources/'Strigov Local MCP.app')
    sign(installer)
    for artifact in [app,installer]:
        subprocess.run(['/usr/bin/codesign','--verify','--strict','--deep',str(artifact)],check=True)
        for file in artifact.rglob('*'):
            if not file.is_file(): continue
            if file.name in ['folders.json','connection.json','tunnel.json','health.url','.env']:
                raise SystemExit('Unexpected user settings in payload: '+file.name)
            content=file.read_bytes()
            if str(pathlib.Path.home()).encode() in content or b'Documents/Claude/Projects' in content:
                raise SystemExit('Personal build path in payload: '+str(file.relative_to(artifact)))
        target=dist/artifact.name
        if target.exists(): shutil.rmtree(target)
        shutil.move(str(artifact),target)
    image_root=dist/'installer-image'
    if image_root.exists(): shutil.rmtree(image_root)
    image_root.mkdir()
    shutil.copytree(dist/'Install Strigov Local MCP.app',image_root/'Install Strigov Local MCP.app')
    shutil.copy2(project/'docs/distribution-readme.txt',image_root/'Read Me.txt')
print(json.dumps({'version':version,'architecture':platform.machine(),'app':str(dist/'Strigov Local MCP.app'),'installer':str(dist/'Install Strigov Local MCP.app')}))
