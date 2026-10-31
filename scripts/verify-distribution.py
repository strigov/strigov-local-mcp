#!/usr/bin/env python3
"""Verify a mounted installer and a clean user installation without touching live services."""
import hashlib, json, os, pathlib, plistlib, re, subprocess, sys, tempfile

image=pathlib.Path(sys.argv[1]).resolve()
project=pathlib.Path(__file__).resolve().parent.parent
def run(*args,**kwargs): return subprocess.run([str(x) for x in args],check=True,capture_output=True,text=True,**kwargs)
with tempfile.TemporaryDirectory(prefix='local-files-distribution-') as temp:
    root=pathlib.Path(temp);mount=root/'mount';mount.mkdir()
    run('/usr/bin/hdiutil','attach','-readonly','-nobrowse','-mountpoint',mount,image)
    try:
        installer=mount/'Install Strigov Local MCP.app/Contents/MacOS/local-files-installer'
        run(installer,'--check')
        destination=root/'Customer Mac/Applications/Strigov Local MCP.app'
        settings=root/'customer-settings'
        env={**os.environ,'LOCAL_FILES_DATA_DIR':str(settings)}
        run(installer,'--install-to',destination,'--no-launch',env=env)
        exe=destination/'Contents/MacOS/local-files'
        info=plistlib.loads((destination/'Contents/Info.plist').read_bytes())
        assert info['CFBundleName']==info['CFBundleDisplayName']=='Strigov Local MCP'
        m=re.search(r'^version\s*=\s*"([^"]+)"',(project/'Cargo.toml').read_text(),re.M)
        if not m: raise SystemExit('Malformed or missing version in Cargo.toml')
        expected_version=m[1]
        assert info.get('CFBundleShortVersionString')==info.get('CFBundleVersion')==expected_version
        assert run(exe,'--version',env=env).stdout.strip()==f'Strigov Local MCP {expected_version}'
        readme=mount/'Read Me.txt' if (mount/'Read Me.txt').exists() else destination/'Contents/Resources/Read Me.txt'
        assert readme.read_text(encoding='utf-8').splitlines()[0].strip()==f'Strigov Local MCP {expected_version}'
        run(exe,'init',env=env)
        config=settings/'folders.json'
        data=json.loads(config.read_text())
        assert data['roots']==[]
        assert not (settings/'connection.json').exists()
        state=json.loads(run(exe,'status',env=env).stdout)
        assert state['connected'] is False and state['live'] is False
        documents=root/'Documents';documents.mkdir();(documents/'sample.txt').write_text('distribution fixture')
        data['roots']=[{'id':'folder_1','path':str(documents),'nickname':'Мои документы'}]
        config.write_text(json.dumps(data))
        before=config.read_bytes()
        connection=settings/'connection.json'
        connection.write_text(json.dumps({'version':1,
            'tunnel_id':'tunnel_0123456789abcdef0123456789abcdef',
            'credential_id':'distribution-test-no-secret'}))
        connection_before=connection.read_bytes()
        run(installer,'--install-to',destination,'--no-launch',env=env)
        run(exe,'init',env=env)
        assert config.read_bytes()==before, 'Update changed folder settings'
        assert connection.read_bytes()==connection_before, 'Update changed connection settings'
        assert len(list(destination.parent.glob('Strigov Local MCP backup *')))==1
        # Updating the same app under its previous bundle name must migrate it,
        # while retaining config bytes and a recoverable copy of the old bundle.
        legacy=destination.with_name('Local Files.app')
        destination.rename(legacy)
        run(installer,'--install-to',destination,'--no-launch',env=env)
        assert destination.exists() and not legacy.exists()
        assert config.read_bytes()==before
        assert connection.read_bytes()==connection_before
        assert len(list(destination.parent.glob('Strigov Local MCP backup */Local Files.app')))==1
        # An unrelated/personal bundle at that name is not the generic 2.1 app.
        (legacy/'Contents').mkdir(parents=True)
        unrelated_info=plistlib.dumps({'CFBundleIdentifier':'example.unrelated.local-files'})
        (legacy/'Contents/Info.plist').write_bytes(unrelated_info)
        run(installer,'--install-to',destination,'--no-launch',env=env)
        assert (legacy/'Contents/Info.plist').read_bytes()==unrelated_info
        assert config.read_bytes()==before
        assert connection.read_bytes()==connection_before
        run('/usr/bin/codesign','--verify','--strict','--deep',destination)
        for path in destination.rglob('*'):
            if path.is_file():
                assert path.name not in ['folders.json','connection.json','tunnel.json','health.url','.env']
                assert str(pathlib.Path.home()).encode() not in path.read_bytes(),str(path)
        test_env={**env,'NATIVE_BINARY':str(exe),'NATIVE_WORKER':'1'}
        tests=run('node','--test','test/native.test.mjs',cwd=project,env=test_env)
        print(tests.stdout)
        runtime_test=run('cargo','test','--locked','--bin','local-files',
            'packaged_runtime_accepts_generated_profile_without_external_account','--','--ignored',
            cwd=project,env={**env,'LOCAL_FILES_TEST_APP':str(destination)})
        print(runtime_test.stdout)
        report={'image':image.name,'sha256':hashlib.sha256(image.read_bytes()).hexdigest(),
            'bytes':image.stat().st_size,'clean_install':True,'empty_initial_roots':True,
            'settings_preserved_on_update':True,'backup_created':True,'codesign_verified':True,
            'display_name':'Strigov Local MCP','previous_name_migration':True,
            'connection_settings_preserved':True,
            'unrelated_legacy_bundle_preserved':True,
            'packaged_worker_mcp_tests':True,'generated_profile_runtime_probe':True,
            'gui_interaction':'NOT_RUN_FOR_RENAME',
            'live_user_settings_changed':False,
            'notarized':False,'other_account_authenticated':'NOT_TESTED_REQUIRES_USER_CREDENTIALS'}
        (image.with_suffix('.verification.json')).write_text(json.dumps(report,indent=2)+'\n')
        print(json.dumps(report,indent=2))
    finally:
        run('/usr/bin/hdiutil','detach',mount)
