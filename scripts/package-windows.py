#!/usr/bin/env python3
"""Build a credential-free Windows ZIP distribution from an explicit file allowlist."""
import argparse, hashlib, json, os, pathlib, re, shutil, subprocess, sys, tempfile, zipfile

PRODUCT = 'app.localfiles.desktop'
PLATFORM = 'windows-amd64'
USER_FILES = {'folders.json', 'connection.json', 'tunnel.json', 'health.url', 'welcome.seen'}


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as stream:
        for block in iter(lambda: stream.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def manifest_entry_path(value):
    """Accept only plain relative POSIX paths without traversal or NTFS streams."""
    path = pathlib.PurePosixPath(value)
    if (not value or '\\' in value or ':' in value or path.is_absolute()
            or any(part in ('', '.', '..') for part in path.parts) or value != str(path)):
        raise ValueError('Unsafe manifest path: ' + repr(value))
    return value


def build_manifest(payload, version):
    files = []
    for file in sorted(payload.rglob('*')):
        if file.is_symlink() or (file.exists() and not (file.is_file() or file.is_dir())):
            raise SystemExit('Unexpected special file in payload: ' + str(file))
        if not file.is_file() or file.name == 'install-manifest.json':
            continue
        relative = manifest_entry_path(file.relative_to(payload).as_posix())
        files.append({'path': relative, 'bytes': file.stat().st_size, 'sha256': sha256(file)})
    files.sort(key=lambda entry: entry['path'])
    return {'schema_version': 1, 'product': PRODUCT, 'version': version,
            'platform': PLATFORM, 'files': files}


def check_private_data(root, forbidden):
    """Reject user settings and the developer's home/build paths in any shipped file."""
    for file in root.rglob('*'):
        if not file.is_file():
            continue
        if file.name in USER_FILES or file.name.startswith('.env'):
            raise SystemExit('Unexpected user settings in payload: ' + file.name)
        content = file.read_bytes()
        for marker in forbidden:
            if marker and (marker.encode() in content or marker.encode('utf-16-le') in content):
                raise SystemExit('Personal path in payload: ' + str(file.relative_to(root)))


def third_party_notices(project):
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--format-version', '1',
         '--filter-platform', 'x86_64-pc-windows-msvc'], cwd=project))
    notices = []
    for package in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
        if package['source'] is None:
            continue
        root = pathlib.Path(package['manifest_path']).parent
        texts = [file.read_text(encoding='utf-8', errors='replace') for file in sorted(root.iterdir())
                 if file.is_file() and file.name.upper().startswith(('LICENSE', 'COPYING'))]
        notices.append(f"{package['name']} {package['version']} ({package.get('license')})\n" + '\n'.join(texts))
    return '\n\n-----\n\n'.join(notices)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--target-dir', default=None, help='Cargo target directory with release binaries')
    args = parser.parse_args()
    if sys.platform != 'win32':
        raise SystemExit('Build the Windows distribution on Windows')
    project = pathlib.Path(__file__).resolve().parent.parent
    version = re.search(r'^version = "([^"]+)"', (project / 'Cargo.toml').read_text(encoding='utf-8'), re.M)[1]
    release = pathlib.Path(args.target_dir or project / 'target') / 'release'
    lock = json.loads((project / 'scripts/tunnel-client.lock.json').read_text(encoding='utf-8'))
    vendor = project / 'artifacts/vendor' / lock['version'] / PLATFORM
    runtime = [p for p in (vendor / 'unpacked').rglob('*')
               if p.is_file() and p.name.lower() in ('tunnel-client.exe', 'tunnel-client-runtime.exe')]
    if len(runtime) != 1:
        raise SystemExit('Fetch the official Windows runtime first (expected exactly one tunnel-client executable)')
    required = [release / 'local-files.exe', release / 'local-files-installer.exe',
                vendor / 'unpacked/LICENSE', vendor / 'unpacked/NOTICE']
    assets = [vendor / a['name'] for a in lock['platforms'][PLATFORM] if not a['name'].endswith('.zip')]
    for file in required + assets:
        if not file.is_file():
            raise SystemExit('Missing build input: ' + str(file))
    for asset in lock['platforms'][PLATFORM]:
        if sha256(vendor / asset['name']) != asset['sha256']:
            raise SystemExit('Pinned runtime digest mismatch: ' + asset['name'])

    dist = project / 'dist'
    dist.mkdir(exist_ok=True)
    name = f'Strigov-Local-MCP-{version}-windows-x64'
    archive = dist / (name + '.zip')
    with tempfile.TemporaryDirectory(prefix='local-files-package-', dir=dist) as temp:
        stage = pathlib.Path(temp) / name
        payload = stage / 'payload'
        licenses = payload / 'licenses'
        licenses.mkdir(parents=True)
        shutil.copy2(release / 'local-files-installer.exe', stage / 'local-files-installer.exe')
        shutil.copy2(project / 'docs/distribution-readme-windows.txt', stage / 'Read Me.txt')
        shutil.copy2(release / 'local-files.exe', payload / 'local-files.exe')
        shutil.copy2(runtime[0], payload / 'tunnel-client.exe')
        for item in ('LICENSE', 'NOTICE'):
            shutil.copy2(vendor / 'unpacked' / item, licenses / f'tunnel-client-{item}.txt')
        for asset in assets:
            shutil.copy2(asset, licenses / asset.name)
        (licenses / 'Third Party Notices.txt').write_text(third_party_notices(project), encoding='utf-8')
        manifest = build_manifest(payload, version)
        (payload / 'install-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n', encoding='utf-8')
        home = str(pathlib.Path.home())
        check_private_data(stage, [home, home.replace('\\', '/'), str(project), str(project).replace('\\', '/')])

        partial = dist / (name + '.zip.partial')
        with zipfile.ZipFile(partial, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as out:
            for file in sorted(stage.rglob('*')):
                if file.is_file():
                    info = zipfile.ZipInfo(file.relative_to(stage).as_posix(), (2026, 1, 1, 0, 0, 0))
                    info.compress_type = zipfile.ZIP_DEFLATED
                    info.external_attr = 0o644 << 16
                    out.writestr(info, file.read_bytes())
        os.replace(partial, archive)
    digest = sha256(archive)
    (dist / (archive.name + '.sha256')).write_text(f'{digest}  {archive.name}\n', encoding='ascii', newline='\n')
    print(json.dumps({'version': version, 'zip': str(archive), 'bytes': archive.stat().st_size, 'sha256': digest}))


if __name__ == '__main__':
    main()
