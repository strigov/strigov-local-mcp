import importlib.util
import json
import pathlib
import re
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

SCRIPT = pathlib.Path(__file__).resolve().parent.parent / 'scripts/make-mcp-experiment.py'
spec = importlib.util.spec_from_file_location('mcp_experiment', SCRIPT)
experiment = importlib.util.module_from_spec(spec)
spec.loader.exec_module(experiment)

PROBES = {
    'image-small.png', 'image-2mb.png', 'image-6mb.png', 'image-12mb.png',
    'page-text.pdf', 'page-scan.pdf', 'pdf-4mb.pdf', 'pdf-12mb.pdf', 'text-200k.txt',
}


def png_pixels(data):
    if data[:8] != b'\x89PNG\r\n\x1a\n':
        raise AssertionError('invalid PNG signature')
    offset, compressed = 8, bytearray()
    while offset < len(data):
        length = struct.unpack_from('>I', data, offset)[0]
        kind = data[offset + 4:offset + 8]
        payload = data[offset + 8:offset + 8 + length]
        crc = struct.unpack_from('>I', data, offset + 8 + length)[0]
        if zlib.crc32(kind + payload) != crc:
            raise AssertionError('invalid PNG CRC')
        if kind == b'IHDR':
            width, height, depth, color, compression, filtering, interlace = struct.unpack('>IIBBBBB', payload)
            if (depth, color, compression, filtering, interlace) != (8, 0, 0, 0, 0):
                raise AssertionError('expected non-interlaced 8-bit grayscale')
        elif kind == b'IDAT':
            compressed.extend(payload)
        offset += length + 12
        if kind == b'IEND':
            break
    rows = zlib.decompress(compressed)
    if len(rows) != height * (width + 1) or offset != len(data):
        raise AssertionError('invalid PNG pixel length')
    # The generator uses PNG filter 0; inspect the decoded center, without pixel-exact comparison.
    if any(rows[y * (width + 1)] != 0 for y in range(height)):
        raise AssertionError('expected unfiltered rows')
    center = b''.join(rows[y * (width + 1) + 1 + width // 2 - 300:
                           y * (width + 1) + 1 + width // 2 + 300]
                      for y in range(height // 2 - 60, height // 2 + 60))
    return width, height, center


def check_pdf(test, data):
    test.assertTrue(data.startswith(b'%PDF-1.'))
    start = re.search(rb'startxref\s+(\d+)\s+%%EOF\s*$', data)
    test.assertIsNotNone(start)
    xref = int(start[1])
    test.assertTrue(data[xref:].startswith(b'xref\n'))
    lines = data[xref:].split(b'\n')
    first, count = map(int, lines[1].split())
    test.assertEqual(first, 0)
    for number, line in enumerate(lines[3:3 + count - 1], 1):
        offset = int(line.split()[0])
        test.assertTrue(data[offset:].startswith(f'{number} 0 obj\n'.encode()))
    test.assertRegex(data, rb'/Type\s*/Pages\b[^>]*?/Count\s+1\b')


class McpExperimentTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.base = pathlib.Path(cls.temp.name)
        cls.probes = cls.base / 'probes'
        cls.cli_result = subprocess.run([sys.executable, '-B', str(SCRIPT), str(cls.probes)],
                                       capture_output=True, text=True, check=True)
        cls.answers_path = cls.base / 'probes.answers.json'
        cls.answers = json.loads(cls.answers_path.read_text(encoding='utf-8'))

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def test_all_probes_sizes_codes_and_separate_answers(self):
        self.assertEqual({p.name for p in self.probes.iterdir()}, PROBES)
        self.assertFalse(self.answers_path.is_relative_to(self.probes))
        self.assertEqual(set(self.answers['codes']), PROBES)
        codes = []
        for name in PROBES:
            self.assertEqual(self.answers['bytes'][name], (self.probes / name).stat().st_size)
            self.assertIn(name, self.cli_result.stdout)
            value = self.answers['codes'][name]
            codes.extend(value.values() if isinstance(value, dict) else [value])
        self.assertEqual(len(set(codes)), len(codes))
        for code in codes:
            self.assertRegex(code, r'^[ACDEFHJKMNPRTUVWXY34679]{6}$')
            self.assertNotIn(code, self.cli_result.stdout)
            self.assertTrue(all(code not in name for name in PROBES))
        for name, mib in [('image-2mb.png', 2), ('image-6mb.png', 6), ('image-12mb.png', 12),
                          ('pdf-4mb.pdf', 4), ('pdf-12mb.pdf', 12)]:
            size = self.answers['bytes'][name]
            self.assertGreater(size, mib * 1024 * 1024 * 0.9)
            self.assertLess(size, mib * 1024 * 1024 * 1.1)

    def test_pngs_decode_and_have_dark_and_light_code_box(self):
        for name in sorted(PROBES):
            if name.endswith('.png'):
                width, height, center = png_pixels((self.probes / name).read_bytes())
                self.assertGreaterEqual(width, 900)
                self.assertGreaterEqual(height, 300)
                self.assertGreater(center.count(0), 1000, name)
                self.assertGreater(center.count(255), 1000, name)
                if name == 'image-small.png':
                    self.assertEqual((width, height), (900, 300))

    def test_pdfs_are_single_page_and_scan_codes_are_only_in_pixels(self):
        for name in sorted(PROBES):
            if not name.endswith('.pdf'):
                continue
            data = (self.probes / name).read_bytes()
            check_pdf(self, data)
            code = self.answers['codes'][name].encode()
            if name == 'page-text.pdf':
                self.assertIn(b'Codeword: ' + code, data)
                self.assertIn(b'/Helvetica', data)
                continue
            self.assertNotIn(code, data)
            self.assertNotIn(b'/Font', data)
            # Find the image stream independently of object order.
            images = [m for m in re.finditer(rb'<<([^<>]*?/Subtype\s*/Image\b[^<>]*?)>>\s*stream\n', data)]
            self.assertEqual(len(images), 1)
            image = images[0]
            header = image[1]
            self.assertIn(b'/FlateDecode', header)
            length = int(re.search(rb'/Length\s+(\d+)', header)[1])
            width = int(re.search(rb'/Width\s+(\d+)', header)[1])
            height = int(re.search(rb'/Height\s+(\d+)', header)[1])
            pixels = zlib.decompress(data[image.end():image.end() + length])
            self.assertEqual(len(pixels), width * height)
            self.assertGreater(pixels.count(0), 1000)
            self.assertGreater(pixels.count(255), 1000)

    def test_text_has_exact_character_count_and_all_markers_at_offsets(self):
        text = (self.probes / 'text-200k.txt').read_text(encoding='utf-8')
        self.assertEqual(len(text), 200_000)
        self.assertIn('Синтетический', text)
        markers = self.answers['codes']['text-200k.txt']
        self.assertEqual(set(markers), {f'MARKER-{n}' for n in range(1, 6)})
        for number, offset in enumerate([1000, 50_000, 100_000, 150_000, 199_000], 1):
            label = f'MARKER-{number}'
            line = f'{label}: {markers[label]}\n'
            self.assertEqual(text.index(line), offset)
            self.assertEqual(text[offset - 1], '\n')
            self.assertEqual(text.count(line), 1)

    def test_existing_and_unsafe_targets_are_refused_without_overwriting(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            target = base / 'existing'
            target.mkdir()
            sentinel = target / 'keep'
            sentinel.write_bytes(b'unchanged')
            with self.assertRaises((OSError, ValueError)):
                experiment.generate(target)
            self.assertEqual(sentinel.read_bytes(), b'unchanged')
            answers = base / 'answers.json'
            answers.write_bytes(b'unchanged')
            with self.assertRaises((OSError, ValueError)):
                experiment.generate(base / 'new', answers)
            self.assertFalse((base / 'new').exists())
            self.assertEqual(answers.read_bytes(), b'unchanged')
            for answer in [base / 'nested' / 'answers.json', base / 'nested' / 'sub' / 'answers.json']:
                with self.assertRaises((OSError, ValueError)):
                    experiment.generate(base / 'nested', answer)
                self.assertFalse((base / 'nested').exists())
            # Resolve directory aliases before checking answer separation.
            (base / 'alias').symlink_to(base, target_is_directory=True)
            with self.assertRaises((OSError, ValueError)):
                experiment.generate(base / 'nested', base / 'alias' / 'nested' / 'answers.json')
            self.assertFalse((base / 'nested').exists())

    def test_custom_answers_cli_and_existing_symlinks(self):
        with tempfile.TemporaryDirectory() as temp:
            base = pathlib.Path(temp)
            target, answers = base / 'new probes', base / 'private answers.json'
            result = subprocess.run([sys.executable, '-B', str(SCRIPT), str(target), '--answers', str(answers)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(set(json.loads(answers.read_text(encoding='utf-8'))['codes']), PROBES)
            self.assertFalse((base / 'new probes.answers.json').exists())
            for name, destination in [('existing-link', target), ('dangling-link', base / 'absent')]:
                link = base / name
                link.symlink_to(destination, target_is_directory=True)
                with self.assertRaises(FileExistsError):
                    experiment.generate(link)
                self.assertTrue(link.is_symlink())
                self.assertFalse((base / (name + '.answers.json')).exists())
            for name, destination in [('key-link', answers), ('dangling-key', base / 'absent-key')]:
                link = base / name
                link.symlink_to(destination)
                with self.assertRaises(FileExistsError):
                    experiment.generate(base / 'not-created', link)
                self.assertTrue(link.is_symlink())
                self.assertFalse((base / 'not-created').exists())


if __name__ == '__main__':
    unittest.main()
