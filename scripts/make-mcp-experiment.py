#!/usr/bin/env python3
"""Create synthetic MCP delivery probes; keep the answer key outside the shared folder."""

import argparse
import json
from pathlib import Path
import secrets
import struct
import zlib

ALPHABET = 'ACDEFHJKMNPRTUVWXY34679'
# Five columns, seven rows; no system fonts, imaging library or external tools.
FONT = {
    'A': ('01110', '10001', '10001', '11111', '10001', '10001', '10001'),
    'C': ('01111', '10000', '10000', '10000', '10000', '10000', '01111'),
    'D': ('11110', '10001', '10001', '10001', '10001', '10001', '11110'),
    'E': ('11111', '10000', '10000', '11110', '10000', '10000', '11111'),
    'F': ('11111', '10000', '10000', '11110', '10000', '10000', '10000'),
    'H': ('10001', '10001', '10001', '11111', '10001', '10001', '10001'),
    'J': ('00111', '00010', '00010', '00010', '10010', '10010', '01100'),
    'K': ('10001', '10010', '10100', '11000', '10100', '10010', '10001'),
    'M': ('10001', '11011', '10101', '10101', '10001', '10001', '10001'),
    'N': ('10001', '11001', '11001', '10101', '10011', '10011', '10001'),
    'P': ('11110', '10001', '10001', '11110', '10000', '10000', '10000'),
    'R': ('11110', '10001', '10001', '11110', '10100', '10010', '10001'),
    'T': ('11111', '00100', '00100', '00100', '00100', '00100', '00100'),
    'U': ('10001', '10001', '10001', '10001', '10001', '10001', '01110'),
    'V': ('10001', '10001', '10001', '10001', '10001', '01010', '00100'),
    'W': ('10001', '10001', '10001', '10101', '10101', '10101', '01010'),
    'X': ('10001', '10001', '01010', '00100', '01010', '10001', '10001'),
    'Y': ('10001', '10001', '01010', '00100', '00100', '00100', '00100'),
    '3': ('11110', '00001', '00001', '01110', '00001', '00001', '11110'),
    '4': ('00010', '00110', '01010', '10010', '11111', '00010', '00010'),
    '6': ('01110', '10000', '10000', '11110', '10001', '10001', '01110'),
    '7': ('11111', '00001', '00010', '00100', '01000', '01000', '01000'),
    '9': ('01110', '10001', '10001', '01111', '00001', '00001', '01110'),
}


def code_image(code, width=900, height=300, noise=False):
    pixels = bytearray(secrets.token_bytes(width * height) if noise else b'\xff' * (width * height))
    scale = 16
    left = (width - (len(code) * 6 - 1) * scale) // 2
    top = (height - 7 * scale) // 2
    for y in range(top - 32, top + 7 * scale + 32):
        start = y * width + left - 32
        pixels[start:start + (len(code) * 6 - 1) * scale + 64] = b'\xff' * ((len(code) * 6 - 1) * scale + 64)
    for index, char in enumerate(code):
        for row, bits in enumerate(FONT[char]):
            for column, bit in enumerate(bits):
                if bit == '1':
                    x = left + (index * 6 + column) * scale
                    for y in range(top + row * scale, top + (row + 1) * scale):
                        pixels[y * width + x:y * width + x + scale] = b'\0' * scale
    return pixels


def png(width, height, pixels):
    def chunk(kind, payload):
        return struct.pack('>I', len(payload)) + kind + payload + struct.pack('>I', zlib.crc32(kind + payload))

    rows = b''.join(b'\0' + pixels[y * width:(y + 1) * width] for y in range(height))
    return (b'\x89PNG\r\n\x1a\n'
            + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 0, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


def stream(payload, entries=b''):
    return b'<< /Length ' + str(len(payload)).encode() + b' ' + entries + b' >>\nstream\n' + payload + b'\nendstream'


def pdf(objects):
    data = bytearray(b'%PDF-1.4\n%\xe2\xe3\xcf\xd3\n')
    offsets = [0]
    for number, body in enumerate(objects, 1):
        offsets.append(len(data))
        data.extend(f'{number} 0 obj\n'.encode() + body + b'\nendobj\n')
    xref = len(data)
    data.extend(f'xref\n0 {len(offsets)}\n0000000000 65535 f \n'.encode())
    for offset in offsets[1:]:
        data.extend(f'{offset:010d} 00000 n \n'.encode())
    data.extend(f'trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode())
    return data


def page_pdf(code, width=900, height=300, noise=False, text=False):
    if text:
        resources = b'<< /Font << /F1 5 0 R >> >>'
        content = f'BT /F1 36 Tf 70 140 Td (Codeword: {code}) Tj ET'.encode()
        object5 = b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>'
    else:
        resources = b'<< /XObject << /Im0 5 0 R >> >>'
        content = f'q {width} 0 0 {height} 0 0 cm /Im0 Do Q'.encode()
        image = zlib.compress(code_image(code, width, height, noise))
        object5 = stream(image, f'/Type /XObject /Subtype /Image /Width {width} /Height {height} '
                               '/ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode'.encode())
    return pdf([
        b'<< /Type /Catalog /Pages 2 0 R >>',
        b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
        f'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Resources '.encode()
        + resources + b' /Contents 4 0 R >>',
        stream(content), object5,
    ])


def text_probe(markers):
    filler = 'Синтетический текст для проверки доставки ответа MCP. Это не документы клиентов.\n'
    parts, position = [], 0
    for number, offset in enumerate([1000, 50_000, 100_000, 150_000, 199_000], 1):
        length = offset - position - 1
        parts.append((filler * (length // len(filler) + 1))[:length] + '\n')
        line = f'MARKER-{number}: {markers[f"MARKER-{number}"]}\n'
        parts.append(line)
        position = offset + len(line)
    length = 200_000 - position
    parts.append((filler * (length // len(filler) + 1))[:length])
    return ''.join(parts)


def generate(new_dir, answers=None):
    target = Path(new_dir).expanduser()
    answers = Path(answers).expanduser() if answers is not None else Path(str(target) + '.answers.json')
    # Refuse links too, including dangling ones; all outputs are newly created.
    if target.exists() or target.is_symlink():
        raise FileExistsError(f'Probe folder already exists: {target}')
    if answers.exists() or answers.is_symlink():
        raise FileExistsError(f'Answers file already exists: {answers}')
    target, answers = target.resolve(), answers.resolve()
    if answers.is_relative_to(target):
        raise ValueError('Answers must be outside the probe folder')
    if not answers.parent.is_dir():
        raise ValueError('Answers parent directory must already exist')
    target.mkdir()
    codes, sizes, used = {}, {}, set()

    def new_code():
        while True:
            code = ''.join(secrets.choice(ALPHABET) for _ in range(6))
            if code not in used:
                used.add(code)
                return code

    def save(name, data, code):
        with (target / name).open('xb') as output:
            output.write(data)
        codes[name], sizes[name] = code, len(data)

    with answers.open('x', encoding='utf-8') as answer_file:
        for name, width, height, noise in [
            ('image-small.png', 900, 300, False),
            ('image-2mb.png', 2048, 1024, True),
            ('image-6mb.png', 2048, 3072, True),
            ('image-12mb.png', 2048, 6144, True),
        ]:
            code = new_code()
            save(name, png(width, height, code_image(code, width, height, noise)), code)
        for name, width, height, noise, text in [
            ('page-text.pdf', 900, 300, False, True),
            ('page-scan.pdf', 900, 300, False, False),
            ('pdf-4mb.pdf', 2048, 2048, True, False),
            ('pdf-12mb.pdf', 2048, 6144, True, False),
        ]:
            code = new_code()
            save(name, page_pdf(code, width, height, noise, text), code)
        markers = {f'MARKER-{n}': new_code() for n in range(1, 6)}
        save('text-200k.txt', text_probe(markers).encode('utf-8'), markers)
        json.dump({'codes': codes, 'bytes': sizes}, answer_file, ensure_ascii=False, indent=2)
        answer_file.write('\n')
    return sizes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('new_dir', type=Path)
    parser.add_argument('--answers', type=Path, help='New answer file outside the probe folder')
    args = parser.parse_args()
    try:
        sizes = generate(args.new_dir, args.answers)
    except (OSError, ValueError) as error:
        parser.exit(1, f'{error}\n')
    print(f'{"File":<22} Bytes')
    for name, size in sizes.items():
        print(f'{name:<22} {size}')


if __name__ == '__main__':
    main()
