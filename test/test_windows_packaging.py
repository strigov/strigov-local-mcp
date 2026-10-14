import importlib.util, pathlib, tempfile, unittest

spec = importlib.util.spec_from_file_location(
    'package_windows', pathlib.Path(__file__).resolve().parent.parent / 'scripts/package-windows.py')
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class WindowsPackagingTest(unittest.TestCase):
    def test_manifest_lists_every_file_sorted_without_itself(self):
        with tempfile.TemporaryDirectory() as temp:
            payload = pathlib.Path(temp)
            (payload / 'licenses').mkdir()
            (payload / 'tunnel-client.exe').write_bytes(b'runtime')
            (payload / 'local-files.exe').write_bytes(b'app')
            (payload / 'licenses/NOTICE.txt').write_text('n')
            (payload / 'install-manifest.json').write_text('{}')
            manifest = package.build_manifest(payload, '2.1.1')
        self.assertEqual([f['path'] for f in manifest['files']],
                         ['licenses/NOTICE.txt', 'local-files.exe', 'tunnel-client.exe'])
        self.assertEqual(manifest['files'][1]['bytes'], 3)
        self.assertEqual(len(manifest['files'][1]['sha256']), 64)
        self.assertEqual((manifest['product'], manifest['platform']), ('app.localfiles.desktop', 'windows-amd64'))

    def test_unsafe_paths_are_rejected(self):
        for bad in ['', '../x', '/x', 'a\\b', 'C:/x', 'a//b', './a', 'a:stream']:
            with self.assertRaises(ValueError, msg=bad):
                package.manifest_entry_path(bad)
        self.assertEqual(package.manifest_entry_path('licenses/a.txt'), 'licenses/a.txt')

    def test_user_settings_and_personal_paths_are_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            (root / 'app.exe').write_bytes('C:\\Users\\dev'.encode('utf-16-le'))
            with self.assertRaises(SystemExit):
                package.check_private_data(root, ['C:\\Users\\dev'])
            (root / 'app.exe').write_bytes(b'clean')
            package.check_private_data(root, ['C:\\Users\\dev'])
            (root / 'connection.json').write_text('{}')
            with self.assertRaises(SystemExit):
                package.check_private_data(root, ['C:\\Users\\dev'])


if __name__ == '__main__':
    unittest.main()
