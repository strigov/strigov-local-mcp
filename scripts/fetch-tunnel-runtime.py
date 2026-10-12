#!/usr/bin/env python3
"""Fetch a pinned official runtime, verifying every file against the release digest."""
import argparse, hashlib, json, pathlib, stat, urllib.request, zipfile

parser=argparse.ArgumentParser()
parser.add_argument('--platform',required=True)
args=parser.parse_args()
project=pathlib.Path(__file__).resolve().parent.parent
lock=json.loads((project/'scripts/tunnel-client.lock.json').read_text())
cache=project/'artifacts/vendor'/lock['version']/args.platform
cache.mkdir(parents=True,exist_ok=True)
for asset in lock['platforms'][args.platform]:
    path=cache/asset['name']
    if not path.exists() or hashlib.sha256(path.read_bytes()).hexdigest()!=asset['sha256']:
        request=urllib.request.Request(asset['url'],headers={'User-Agent':'Local-Files-release-builder'})
        with urllib.request.urlopen(request,timeout=60) as response: data=response.read(64*1024*1024+1)
        if len(data)>64*1024*1024 or hashlib.sha256(data).hexdigest()!=asset['sha256']:
            raise SystemExit('Release digest mismatch: '+asset['name'])
        path.write_bytes(data)
    if path.suffix=='.zip':
        with zipfile.ZipFile(path) as archive:
            for info in archive.infolist():
                member=pathlib.PurePosixPath(info.filename)
                if member.is_absolute() or '..' in member.parts or stat.S_ISLNK(info.external_attr>>16):
                    raise SystemExit('Unsafe archive member')
                if info.file_size>64*1024*1024: raise SystemExit('Oversized archive member')
            archive.extractall(cache/'unpacked')
print(cache)
