import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, symlink, rm, rename, open } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import test from 'node:test';
import { deflateSync, deflateRawSync, crc32 } from 'node:zlib';
import { execFileSync } from 'node:child_process';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';

const binary=path.resolve(process.env.NATIVE_BINARY || (process.platform==='win32'?'target/release/local-files.exe':'target/release/local-files'));
async function fixture(t) {
  const base=await mkdtemp(path.join(os.tmpdir(),'local-files-rust-'));
  const work=path.join(base,'work'),cowork=path.join(base,'cowork');
  await mkdir(work);await mkdir(cowork);
  const configPath=path.join(base,'folders.json');
  const config={version:1,roots:[{id:'work',path:work},{id:'cowork',path:cowork}],next_root_id:1};
  await writeFile(configPath,JSON.stringify(config));
  await writeFile(path.join(work,'Привет.md'),'Первая строка\r\nИскомая фраза 😀\r\nТретья строка\n');
  await writeFile(path.join(work,'.env'),'SECRET=not-readable');
  await writeFile(path.join(cowork,'outside.txt'),'OUTSIDE');
  await symlink(path.join(cowork,'outside.txt'),path.join(work,'escape.txt'));
  await symlink(path.join(work,'.env'),path.join(work,'alias.txt'));
  const transport=new StdioClientTransport({command:binary,args:[process.env.NATIVE_WORKER==='1'?'worker':'serve','--config',configPath],stderr:'pipe'});
  let stderr='';transport.stderr?.on('data',chunk=>stderr+=chunk);
  const client=new Client({name:'native-verification',version:'1'});
  t.after(async()=>{await client.close();await rm(base,{recursive:true,force:true});});
  try {await client.connect(transport);}catch(e){throw new Error(`${e}\n${stderr}`);}
  const call=async(name,args={})=>client.callTool({name,arguments:args},undefined,{timeout:5000});
  return {base,work,cowork,configPath,config,call,client};
}
test('native MCP retains tools and read-only annotations',async t=>{
  const f=await fixture(t);const {tools}=await f.client.listTools();
  assert.deepEqual(tools.map(t=>t.name).sort(),['fetch','get_file_info','inspect_document','list_directory','list_roots','read_document','read_file','read_text_file','search','search_files','search_text']);
  assert.ok(tools.every(t=>t.annotations.readOnlyHint && !t.annotations.destructiveHint));
  const roots=(await f.call('list_roots')).structuredContent.roots;
  assert.deepEqual(roots.map(r=>r.name),['work','cowork']);assert.ok(roots.every(r=>!r.path.startsWith('/')));
  const read=(await f.call('read_text_file',{root:'work',path:'Привет.md',startLine:2,endLine:2})).structuredContent;
  assert.equal(read.text,'Искомая фраза 😀');assert.equal(read.totalLines,4);
});
test('new installations share no folders even when legacy environment paths exist',async t=>{
  const f=await fixture(t);
  const clean=path.join(f.base,'new-user','folders.json');
  execFileSync(binary,['init','--config',clean],{env:{...process.env,MCP_WORK_ROOT:f.work,MCP_COWORK_ROOT:f.cowork}});
  const {readFile}=await import('node:fs/promises');
  const config=JSON.parse(await readFile(clean,'utf8'));
  assert.deepEqual(config.roots,[]);
  const status=JSON.parse(execFileSync(binary,['status','--config',clean],{encoding:'utf8'}));
  assert.equal(status.connected,false);assert.equal(status.live,false);
  config.roots.push({id:'documents',path:f.work,nickname:'Мои документы'});
  await writeFile(clean,JSON.stringify(config));
  const before=await readFile(clean,'utf8');
  execFileSync(binary,['init','--config',clean]);
  assert.equal(await readFile(clean,'utf8'),before);
});
test('path escape, Windows paths, secrets and symlinks are rejected',async t=>{
  const f=await fixture(t);
  for(const p of ['../cowork/outside.txt','..\\cowork\\outside.txt','/etc/passwd','C:\\Windows\\win.ini','file.txt:secret','.env','escape.txt','alias.txt']) {
    const r=await f.call('read_text_file',{root:'work',path:p});assert.equal(r.isError,true,p);
    assert.ok(!JSON.stringify(r).includes('SECRET=') && !JSON.stringify(r).includes('OUTSIDE'),p);
  }
});
test('UTF-8 search, stable fetch IDs and binary resources',async t=>{
  const f=await fixture(t);
  const search=(await f.call('search_text',{root:'work',query:'ИСКОМАЯ'})).structuredContent;
  assert.equal(search.results.length,1);assert.equal(search.results[0].line,2);
  const results=(await f.call('search',{query:'Привет'})).structuredContent.results;
  assert.equal(results[0].id,'work:Привет.md');
  assert.match((await f.call('fetch',{id:results[0].id})).structuredContent.text,/Искомая/);
  await writeFile(path.join(f.work,'sample.bin'),Buffer.from([0xff,0x00,0x01]));
  const binary=await f.call('read_file',{root:'work',path:'sample.bin'});
  const resource=binary.content.find(c=>c.type==='resource');
  assert.equal(resource.resource.blob,'/wAB');
  assert.equal(binary.content[0].type,'text');assert.equal(binary.structuredContent.blob,undefined);
});
test('read_file returns PNG image content with a text summary and bounded passthrough',async t=>{
  const f=await fixture(t);
  const png=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGNgAAAAAgABSK+kcQAAAABJRU5ErkJggg==','base64');
  await writeFile(path.join(f.work,'pixel.png'),png);
  const result=await f.call('read_file',{root:'work',path:'pixel.png'});
  assert.equal(result.isError,undefined);
  assert.equal(result.content[0].type,'text');assert.match(result.content[0].text,/work\/pixel.png.*image\/png/);
  assert.deepEqual(result.content.find(c=>c.type==='image'),{type:'image',data:png.toString('base64'),mimeType:'image/png'});
  assert.equal(result.structuredContent.kind,'image');assert.equal(result.structuredContent.size,png.length);
  assert.equal(result.structuredContent.data,undefined);assert.equal(result.structuredContent.blob,undefined);
  assert.ok(!JSON.stringify(result).includes(f.work));
  for(const ext of ['PNG','jpg','jpeg','gif','webp']) {
    await writeFile(path.join(f.work,`invalid.${ext}`),'GIF89a');
    const read=await f.call('read_file',{root:'work',path:`invalid.${ext}`});
    assert.equal(read.structuredContent.error.code,'DOCUMENT_INVALID');
    assert.equal(read.content.some(c=>c.type==='image'),false);
  }
  const huge=await open(path.join(f.work,'oversized.png'),'w');await huge.truncate(128*1024*1024+1);await huge.close();
  assert.equal((await f.call('read_file',{root:'work',path:'oversized.png'})).structuredContent.error.code,'FILE_TOO_LARGE');
  assert.equal((await f.call('read_text_file',{root:'work',path:'pixel.png'})).structuredContent.error.code,'NOT_TEXT');
});
function syntheticPng(width,height) {
  const crc=bytes=>{let c=0xffffffff;for(const b of bytes){c^=b;for(let i=0;i<8;i++)c=(c>>>1)^(0xedb88320&-(c&1));}return (c^0xffffffff)>>>0;};
  const chunk=(type,bytes)=>{const name=Buffer.from(type),size=Buffer.alloc(4),check=Buffer.alloc(4);size.writeUInt32BE(bytes.length);check.writeUInt32BE(crc(Buffer.concat([name,bytes])));return Buffer.concat([size,name,bytes,check]);};
  const head=Buffer.alloc(13);head.writeUInt32BE(width);head.writeUInt32BE(height,4);head[8]=8;
  const pixels=Buffer.alloc((width+1)*height);
  for(let y=0;y<height;y++)pixels.fill(255,y*(width+1)+1+Math.floor(width/2),(y+1)*(width+1));
  return Buffer.concat([Buffer.from([137,80,78,71,13,10,26,10]),chunk('IHDR',head),chunk('IDAT',deflateSync(pixels)),chunk('IEND',Buffer.alloc(0))]);
}
test('visual A+pdf schema, image downscale, crop, inspection and argument bounds',async t=>{
  const f=await fixture(t);const tool=(await f.client.listTools()).tools.find(t=>t.name==='read_file');
  assert.equal(tool.inputSchema.additionalProperties,false);assert.equal(tool.inputSchema.properties.page.maximum,1000000);
  assert.equal(tool.inputSchema.properties.region.additionalProperties,false);
  await writeFile(path.join(f.work,'large.png'),syntheticPng(4096,1024));
  const read=(await f.call('read_file',{root:'work',path:'large.png'}));
  assert.equal(read.structuredContent.kind,'image');assert.equal(read.structuredContent.width,2048);assert.equal(read.structuredContent.height,512);
  assert.equal(read.structuredContent.sourceWidth,4096);assert.equal(read.structuredContent.passthrough,false);
  assert.ok(Buffer.from(read.content.find(c=>c.type==='image').data,'base64').length<=1494174);
  const crop=(await f.call('read_file',{root:'work',path:'large.png',region:{x:0.75,y:0,width:0.25,height:1}})).structuredContent;
  assert.equal(crop.width,1024);assert.equal(crop.height,1024);assert.equal(crop.region.x,0.75);
  const info=(await f.call('inspect_document',{root:'work',path:'large.png'})).structuredContent;
  assert.equal(info.width,4096);assert.equal(info.height,1024);assert.equal(info.pages,1);
  for(const region of [{x:0.8,y:0,width:0.3,height:1},{x:0,y:0,width:0,height:1}])
    assert.equal((await f.call('read_file',{root:'work',path:'large.png',region})).structuredContent.error.code,'INVALID_REGION');
  assert.equal((await f.call('read_file',{root:'work',path:'large.png',page:2})).structuredContent.error.code,'PAGE_OUT_OF_RANGE');
  for(const extra of [{page:1},{region:{x:0,y:0,width:1,height:1}}])
    assert.equal((await f.call('read_file',{root:'work',path:'Привет.md',...extra})).structuredContent.error.code,'INVALID_ARGUMENT');
  await writeFile(path.join(f.work,'scan.heic'),'synthetic');
  assert.equal((await f.call('read_file',{root:'work',path:'scan.heic',page:1})).structuredContent.error.code,'UNSUPPORTED_FORMAT');
});
test('PDF page views prefer scan images and return one-page PDF fallback for visible text',async t=>{
  const f=await fixture(t);await writeFile(path.join(f.work,'view.pdf'),syntheticPdf());
  const scan=await f.call('read_file',{root:'work',path:'view.pdf',page:2});
  assert.equal(scan.structuredContent.kind,'image');assert.equal(scan.structuredContent.page,2);assert.equal(scan.structuredContent.pages,2);
  assert.equal(scan.structuredContent.width,1000);assert.ok(scan.content.some(c=>c.type==='image'));
  const crop=(await f.call('read_file',{root:'work',path:'view.pdf',page:2,region:{x:0,y:0,width:0.5,height:1}})).structuredContent;
  assert.equal(crop.width,500);assert.equal(crop.height,1000);
  const fallback=await f.call('read_file',{root:'work',path:'view.pdf',page:1});
  assert.equal(fallback.structuredContent.kind,'binary');assert.equal(fallback.structuredContent.page,1);assert.equal(fallback.structuredContent.pageCount,1);
  assert.match(fallback.structuredContent.uri,/#page=1$/);assert.ok(fallback.structuredContent.size<=262137);
  const pdf=Buffer.from(fallback.content.find(c=>c.type==='resource').resource.blob,'base64');
  assert.equal(pdf.subarray(0,5).toString(),'%PDF-');assert.equal(fallback.structuredContent.blob,undefined);
  assert.match(fallback.structuredContent.note,/Allow file materialization/);
  assert.equal((await f.call('read_file',{root:'work',path:'view.pdf'})).structuredContent.kind,'document');
  assert.equal((await f.call('read_file',{root:'work',path:'view.pdf',page:3})).structuredContent.error.code,'PAGE_OUT_OF_RANGE');
});
test('config revocation applies to the same live session and invalid config fails closed',async t=>{
  const f=await fixture(t);
  assert.equal((await f.call('read_text_file',{root:'work',path:'Привет.md'})).isError,undefined);
  f.config.roots=f.config.roots.filter(r=>r.id!=='work');
  await writeFile(f.configPath+'.new',JSON.stringify(f.config));await rename(f.configPath+'.new',f.configPath);
  assert.equal((await f.call('read_text_file',{root:'work',path:'Привет.md'})).structuredContent.error.code,'UNKNOWN_ROOT');
  await writeFile(f.configPath,'broken');
  assert.equal((await f.call('list_roots')).structuredContent.error.code,'CONFIG_UNAVAILABLE');
});
test('folder nicknames update in a live session while old document IDs remain valid',async t=>{
  const f=await fixture(t);
  const oldResult=(await f.call('search',{query:'Привет'})).structuredContent.results[0];
  const toolsBefore=await f.client.listTools();
  f.config.roots[0].nickname='Работа (OneDrive)';
  await writeFile(f.configPath+'.new',JSON.stringify(f.config));await rename(f.configPath+'.new',f.configPath);
  const roots=(await f.call('list_roots')).structuredContent.roots;
  assert.equal(roots[0].name,'Работа (OneDrive)');assert.equal(roots[0].id,'work');
  assert.equal(roots[0].path,'work');assert.match(roots[0].uri,/\/work\/$/);
  for(const name of ['Работа (OneDrive)','РАБОТА (ONEDRIVE)','work']) {
    const read=await f.call('read_text_file',{root:name,path:'Привет.md'});
    assert.match(read.structuredContent.text,/Искомая/);
    assert.equal((await f.call('list_directory',{root:name})).isError,undefined);
    assert.equal((await f.call('search_text',{root:name,query:'Искомая'})).structuredContent.results.length,1);
    assert.equal((await f.call('search_files',{root:name,query:'Привет'})).structuredContent.results.length,1);
    assert.equal((await f.call('get_file_info',{root:name,path:'Привет.md'})).isError,undefined);
    assert.equal((await f.call('read_file',{root:name,path:'Привет.md'})).isError,undefined);
    assert.equal((await f.call('read_text_file',{root:name,path:'../cowork/outside.txt'})).isError,true);
  }
  assert.match((await f.call('fetch',{id:oldResult.id})).structuredContent.text,/Искомая/);
  assert.equal((await f.call('search',{query:'Привет'})).structuredContent.results[0].id,oldResult.id);
  f.config.roots[0].nickname='Юридические документы';
  await writeFile(f.configPath,JSON.stringify(f.config));
  assert.equal((await f.call('read_text_file',{root:'Работа (OneDrive)',path:'Привет.md'})).structuredContent.error.code,'UNKNOWN_ROOT');
  assert.match((await f.call('read_text_file',{root:'Юридические документы',path:'Привет.md'})).structuredContent.text,/Искомая/);
  assert.match((await f.call('fetch',{id:oldResult.id})).structuredContent.text,/Искомая/);
  assert.deepEqual(await f.client.listTools(),toolsBefore);
});
test('offline roots do not break other folders and are disclosed',async t=>{
  const f=await fixture(t);await rename(f.cowork,f.cowork+'.offline');
  const roots=(await f.call('list_roots')).structuredContent.roots;
  assert.equal(roots.find(r=>r.name==='cowork').available,false);
  const search=(await f.call('search_files',{query:'Привет'})).structuredContent;
  assert.equal(search.results.length,1);assert.deepEqual(search.unavailableRoots,['cowork']);
  assert.equal((await f.call('read_text_file',{root:'cowork',path:'outside.txt'})).structuredContent.error.code,'ROOT_UNAVAILABLE');
});
test('output and file bounds survive multibyte text and oversized files',async t=>{
  const f=await fixture(t);await writeFile(path.join(f.work,'emoji.txt'),'😀'.repeat(50));
  const read=(await f.call('read_text_file',{root:'work',path:'emoji.txt',maxChars:3})).structuredContent;
  assert.equal(read.text,'😀😀😀');assert.equal(read.truncated,true);
  await writeFile(path.join(f.work,'big.txt'),Buffer.alloc(6*1024*1024,65));
  const big=(await f.call('read_text_file',{root:'work',path:'big.txt',maxChars:20})).structuredContent;
  assert.equal(big.text,'A'.repeat(20));assert.equal(big.truncated,true);
  const huge=await open(path.join(f.work,'too-large.txt'),'w');
  await huge.truncate(64*1024*1024+1);await huge.close();
  for(const name of ['read_text_file','read_file','read_document']) {
    assert.equal((await f.call(name,{root:'work',path:'too-large.txt'})).structuredContent.error.code,'FILE_TOO_LARGE');
  }
  await rm(path.join(f.work,'too-large.txt'));
  assert.equal((await f.call('search_text',{query:'ZZZZ'})).structuredContent.skippedLargeFiles,1);
  const search=(await f.call('search',{query:'ZZZZ'})).structuredContent;
  assert.equal(search.skippedLargeFiles,1);assert.equal(search.truncated,false);
});
test('connector search discloses result limits',async t=>{
  const f=await fixture(t);
  await Promise.all(Array.from({length:25},(_,i)=>writeFile(path.join(f.work,`match-${i}.txt`),'content')));
  const search=(await f.call('search',{query:'match'})).structuredContent;
  assert.equal(search.results.length,20);assert.equal(search.truncated,true);
});
test('special files fail promptly without blocking following requests',{skip:process.platform==='win32'},async t=>{
  const f=await fixture(t);execFileSync('mkfifo',[path.join(f.work,'pipe.txt')]);
  await symlink('pipe.txt',path.join(f.work,'pipe-alias.txt'));
  for(const p of ['pipe.txt','pipe-alias.txt']) {
    assert.equal((await f.call('read_text_file',{root:'work',path:p})).isError,true);
  }
  await symlink('Привет.md',path.join(f.work,'safe.txt'));
  assert.match((await f.call('read_text_file',{root:'work',path:'safe.txt'})).structuredContent.text,/Искомая/);
  await symlink('.env',path.join(f.work,'relative-secret.txt'));
  assert.equal((await f.call('read_text_file',{root:'work',path:'relative-secret.txt'})).isError,true);
  assert.equal((await f.call('list_roots')).isError,undefined);
});

function syntheticPdf(phrase='Synthetic native PDF text for the first page.') {
  const image=deflateSync(Buffer.alloc(1000000,200));
  const text=Buffer.from(`BT /F 12 Tf (${phrase}) Tj ET`);
  const paint=Buffer.from('q 600 0 0 800 0 0 cm /Im Do Q');
  const stream=(dict,data)=>Buffer.concat([Buffer.from(`<< ${dict} /Length ${data.length} >>\nstream\n`),data,Buffer.from('\nendstream')]);
  const objects=[
    Buffer.from('<< /Type /Catalog /Pages 2 0 R >>'),
    Buffer.from('<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] /MediaBox [0 0 600 800] >>'),
    Buffer.from('<< /Type /Page /Parent 2 0 R /Resources << /Font << /F 5 0 R >> >> /Contents 6 0 R >>'),
    Buffer.from('<< /Type /Page /Parent 2 0 R /Resources << /XObject << /Im 7 0 R >> >> /Contents 8 0 R >>'),
    Buffer.from('<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>'),
    stream('',text),
    stream('/Type /XObject /Subtype /Image /Width 1000 /Height 1000 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode',image),
    stream('',paint),
  ];
  const parts=[Buffer.from('%PDF-1.5\n')],offsets=[0];let length=parts[0].length;
  for(const [index,object] of objects.entries()) {
    offsets.push(length);const part=Buffer.concat([Buffer.from(`${index+1} 0 obj\n`),object,Buffer.from('\nendobj\n')]);
    parts.push(part);length+=part.length;
  }
  const xref=length;
  parts.push(Buffer.from(`xref\n0 ${offsets.length}\n0000000000 65535 f \n${offsets.slice(1).map(o=>`${String(o).padStart(10,'0')} 00000 n \n`).join('')}trailer\n<< /Size ${offsets.length} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`));
  return Buffer.concat(parts);
}
test('sorted folder and search cursors cover all entries once and bind their request',async t=>{
  const f=await fixture(t);await mkdir(path.join(f.work,'paged'));
  await Promise.all(Array.from({length:25},(_,i)=>writeFile(path.join(f.work,'paged',`match-${String(i).padStart(2,'0')}.txt`),'needle match')));
  for(const [tool,field,limit,query] of [['search_files','results',10,'match'],['search_text','results',10,'needle'],['list_directory','entries',10,undefined]]) {
    const args={root:'work',path:'paged',limit,...(query?{query}:{})};const paths=[];let calls=0,first;
    do {
      const result=(await f.call(tool,args)).structuredContent;
      assert.equal(result.truncated,!!result.nextCursor);paths.push(...result[field].map(e=>e.relativePath));calls++;
      if(!first)first=result.nextCursor;
      if(!result.nextCursor)break;args.cursor=result.nextCursor;
      assert.ok(calls<10);
    }while(true);
    assert.equal(calls,3);assert.equal(paths.length,25);assert.equal(new Set(paths).size,25);assert.deepEqual(paths,[...paths].sort());
    const changed=await f.call(tool,{...args,cursor:first,path:''});assert.equal(changed.structuredContent.error.code,'INVALID_CURSOR');
  }
  await mkdir(path.join(f.work,'paged','a-dir'));await writeFile(path.join(f.work,'paged','a-dir','child.txt'),'needle');
  const listing=(await f.call('list_directory',{root:'work',path:'paged',depth:2})).structuredContent.entries;
  assert.deepEqual(listing.slice(0,3).map(e=>e.relativePath),['paged/a-dir','paged/a-dir/child.txt','paged/match-00.txt']);
  // A token containing a foreign path never opens that path; only disk names are compared.
  const first=(await f.call('list_directory',{root:'work',path:'paged',limit:1})).structuredContent;
  const original=Buffer.from(first.nextCursor,'base64url');
  assert.equal(original[10],0);assert.deepEqual([...original.subarray(11,14)],[0,0,2]);
  const components=['missing-root','../../outside'];
  const forged=Buffer.concat([original.subarray(0,13),Buffer.from([components.length]),...components.flatMap(name=>[Buffer.from([Buffer.byteLength(name)]),Buffer.from(name)])]);
  assert.equal((await f.call('list_directory',{root:'work',path:'paged',cursor:forged.toString('base64url')})).structuredContent.error.code,'INVALID_CURSOR');
});
test('quote-named directory cursor chains cover every entry once',{skip:process.platform==='win32'},async t=>{
  const f=await fixture(t);const base=path.join(f.work,'quoted');await mkdir(base);
  const name='"'.repeat(200);const directories=[],files=[];let relative='quoted';
  for(let i=1;i<=4;i++) {
    relative+=`/${name}`;directories.push(relative);await mkdir(path.join(f.work,relative));
    const file=`${relative}/hit-${i}.txt`;files.push(file);await writeFile(path.join(f.work,file),'quote-needle');
  }
  for(const [tool,field,limit,query] of [['list_directory','entries',4,undefined],['search_files','results',1,'.txt'],['search_text','results',1,'quote-needle']]) {
    const args={root:'work',path:'quoted',limit,...(tool==='list_directory'?{depth:5}:{}),...(query?{query}:{})};const found=[];
    for(let calls=0;;calls++) {
      assert.ok(calls<20);const result=await f.call(tool,args);assert.equal(result.isError,undefined);
      const page=result.structuredContent;assert.equal(page.skippedTooDeep,0);
      found.push(...page[field].map(entry=>entry.relativePath));
      if(!page.nextCursor)break;
      assert.ok(page.nextCursor.length<=2048);args.cursor=page.nextCursor;
    }
    const expected=tool==='list_directory'?[...directories,...files]:files;
    assert.deepEqual(found,[...expected].sort());assert.equal(new Set(found).size,expected.length);
  }
});
test('deep directory walks disclose skipped descent once per cursor chain',{skip:process.platform==='win32'},async t=>{
  const f=await fixture(t);const base=path.join(f.work,'deep-tree');await mkdir(base);
  const name='d'.repeat(255);
  const tree=action=>execFileSync(process.execPath,['-e',`
    const fs=require('node:fs');const name='d'.repeat(255);
    if(process.argv[1]==='create') {
      for(let depth=1;depth<=6;depth++) { fs.mkdirSync(name);process.chdir(name);if(depth===5)fs.writeFileSync('mid.txt','deep-needle'); }
      fs.writeFileSync('deep.txt','deep-needle');
    } else {
      for(let depth=1;depth<=6;depth++)process.chdir(name);
      fs.unlinkSync('deep.txt');
      for(let depth=6;depth>=1;depth--) { process.chdir('..');fs.rmdirSync(name);if(depth===6)fs.unlinkSync('mid.txt'); }
    }
  `,action],{cwd:base});
  tree('create');
  try {
    await writeFile(path.join(base,'z.txt'),'deep-needle');
    const expected=[`deep-tree/${Array(5).fill(name).join('/')}/mid.txt`,'deep-tree/z.txt'];
    for(const [tool,query] of [['search_files','.txt'],['search_text','deep-needle']]) {
      const args={root:'work',path:'deep-tree',query,limit:1};let skipped=0;const found=[];
      for(let calls=0;;calls++) {
        assert.ok(calls<10);const result=await f.call(tool,args);assert.equal(result.isError,undefined);
        const page=result.structuredContent;assert.ok(Number.isInteger(page.skippedTooDeep));skipped+=page.skippedTooDeep;
        found.push(...page.results.map(entry=>entry.relativePath));
        if(!page.nextCursor)break;
        assert.ok(page.nextCursor.length<=2048);args.cursor=page.nextCursor;
      }
      assert.deepEqual(found,expected);assert.equal(new Set(found).size,2);assert.equal(skipped,1);
    }
    const listing=(await f.call('list_directory',{root:'work',path:'deep-tree',depth:5})).structuredContent;
    assert.equal(listing.skippedTooDeep,0);
    assert.ok(listing.entries.some(entry=>entry.relativePath===`deep-tree/${Array(5).fill(name).join('/')}`));
    assert.ok(!listing.entries.some(entry=>entry.relativePath.endsWith('/deep.txt')));
  } finally { tree('cleanup'); }
});
async function cyrillicCursorCase(t,baseName,dirname,filesByDepth,fittingFiles,expectedSkipped) {
  const f=await fixture(t);const base=path.join(f.work,baseName);await mkdir(base);
  const tree=action=>execFileSync(process.execPath,['-e',`
    const fs=require('node:fs');const dirname=process.argv[1],files=JSON.parse(process.argv[2]);
    if(process.argv[3]==='create') {
      for(const names of files) { fs.mkdirSync(dirname);process.chdir(dirname);for(const name of names)fs.writeFileSync(name,'cyrillic-needle'); }
    } else {
      for(const names of files)process.chdir(dirname);
      for(let depth=files.length-1;depth>=0;depth--) { for(const name of files[depth])fs.unlinkSync(name);process.chdir('..');fs.rmdirSync(dirname); }
    }
  `,dirname,JSON.stringify(filesByDepth),action],{cwd:base});
  const location=(depth,name)=>`${baseName}/${Array(depth).fill(dirname).join('/')}${name?`/${name}`:''}`;
  tree('create');
  try {
    const directories=filesByDepth.map((_,index)=>location(index+1));
    const expectedFiles=fittingFiles.map(([depth,name])=>location(depth,name));
    for(const [tool,field,query] of [['list_directory','entries',undefined],['search_files','results','.txt'],['search_text','results','cyrillic-needle']]) {
      const args={root:'work',path:baseName,limit:1,...(tool==='list_directory'?{depth:5}:{}),...(query?{query}:{})};
      const found=[],lengths=[];let skipped=0,calls=0;
      for(;;) {
        assert.ok(calls++<20);const result=await f.call(tool,args);assert.equal(result.isError,undefined);
        const page=result.structuredContent;assert.ok(Number.isInteger(page.skippedTooDeep));skipped+=page.skippedTooDeep;
        found.push(...page[field].map(entry=>entry.relativePath));
        if(!page.nextCursor)break;
        lengths.push(page.nextCursor.length);
        assert.ok(page.nextCursor.length<=2048,`${baseName}/${tool}: cursor length ${page.nextCursor.length}`);
        args.cursor=page.nextCursor;
      }
      const expected=tool==='list_directory'?[...directories,...expectedFiles]:expectedFiles;
      assert.deepEqual(found,[...expected].sort());assert.equal(new Set(found).size,expected.length);assert.equal(skipped,expectedSkipped);
      t.diagnostic(JSON.stringify({scenario:baseName,tool,calls,cursorLengths:lengths,entries:found.length,skippedTooDeep:skipped,complete:true}));
    }
  } finally { tree('cleanup'); }
}
test('Cyrillic tail skips preserve complete cursor chains',{skip:process.platform!=='darwin'},async t=>{
  const long1='я'.repeat(248)+'-1.txt',long2='я'.repeat(248)+'-2.txt';
  await cyrillicCursorCase(t,'cyr-a','я'.repeat(255),[[],['short.txt',long1,long2]],[[2,'short.txt']],2);
});
test('Cyrillic mid-walk skips preserve complete cursor chains',{skip:process.platform!=='darwin'},async t=>{
  const name='я'.repeat(200)+'.txt';
  await cyrillicCursorCase(t,'cyr-b','я'.repeat(140),Array.from({length:4},()=>[name]),[1,2,3].map(depth=>[depth,name]),1);
});
test('document text, inspection, fetch and read_file use the PDF text layer',async t=>{
  const f=await fixture(t);await writeFile(path.join(f.work,'synthetic.pdf'),syntheticPdf());
  const doc=(await f.call('read_document',{root:'work',path:'synthetic.pdf'})).structuredContent;
  assert.equal(doc.kind,'document');assert.equal(doc.format,'pdf');assert.equal(doc.pageCount,2);
  assert.match(doc.text,/source: work\/synthetic.pdf/);assert.match(doc.text,/page 1 · text_layer/);
  assert.match(doc.text,/page 2 · no text layer/);assert.deepEqual(doc.units.map(u=>u.page),[1,2]);
  assert.equal(doc.units[0].source,'text_layer');assert.equal(doc.units[1].kind,'no_text');
  const info=(await f.call('inspect_document',{root:'work',path:'synthetic.pdf'})).structuredContent;
  assert.equal(info.pages.text,'1');assert.equal(info.pages.no_text,'2');assert.equal(info.pageCount,2);assert.equal(info.complete,true);
  assert.match((await f.call('fetch',{id:'work:synthetic.pdf'})).structuredContent.text,/native PDF text/);
  const read=await f.call('read_file',{root:'work',path:'synthetic.pdf'});assert.equal(read.structuredContent.kind,'document');assert.ok(!read.content.some(c=>c.type==='resource'));
  assert.equal((await f.call('get_file_info',{root:'work',path:'synthetic.pdf'})).structuredContent.format,'pdf');
  const page=(await f.call('read_document',{root:'work',path:'synthetic.pdf',startPage:2,endPage:2})).structuredContent;
  assert.deepEqual(page.units.map(u=>u.page),[2]);
  assert.equal((await f.call('read_document',{root:'work',path:'synthetic.pdf',startPage:3})).structuredContent.error.code,'PAGE_OUT_OF_RANGE');
  await writeFile(path.join(f.work,'invalid.pdf'),Buffer.from([0xff,0,1]));
  const invalid=await f.call('read_file',{root:'work',path:'invalid.pdf'});
  assert.equal(invalid.structuredContent.error.code,'DOCUMENT_INVALID');assert.ok(!invalid.content.some(c=>c.type==='resource'));
});
test('document continuation progresses at maxChars=1 and rejects stale files',async t=>{
  const f=await fixture(t);const text='Текст 😀 продолжения';await writeFile(path.join(f.work,'cursor.txt'),text);
  const args={root:'work',path:'cursor.txt',maxChars:1};let recovered='',first;
  for(let i=0;i<100;i++) {
    const result=(await f.call('read_document',args)).structuredContent;
    assert.equal(result.units.length,1);assert.equal(result.units[0].chars,1);
    recovered+=result.text.slice(result.text.indexOf('\n')+1);
    if(!first)first=result.nextCursor;
    if(!result.nextCursor)break;args.cursor=result.nextCursor;
  }
  assert.equal(recovered,text);
  await writeFile(path.join(f.work,'cursor.txt'),'changed');
  assert.equal((await f.call('read_document',{root:'work',path:'cursor.txt',cursor:first})).structuredContent.error.code,'STALE_CURSOR');
  await writeFile(path.join(f.work,'invalid-utf8.txt'),Buffer.from([65,10,0xff]));
  for(const tool of ['read_document','read_text_file']) {
    assert.equal((await f.call(tool,{root:'work',path:'invalid-utf8.txt',maxChars:1})).structuredContent.error.code,'NOT_TEXT');
  }
});
test('unsupported formats give conversion hints before binary fallback',async t=>{
  const f=await fixture(t);
  for(const [name,hint] of [['old.doc',/save as DOCX or PDF/],['photo.heic',/convert to JPEG\/PNG/],['raw.rtf',/save as DOCX or PDF/],['old.rar',/extract it or repack as ZIP/]]) {
    await writeFile(path.join(f.work,name),'{\\rtf1 raw text}');
    for(const tool of ['read_file','read_document','inspect_document']) {
      const result=await f.call(tool,{root:'work',path:name});
      assert.equal(result.structuredContent.error.code,'UNSUPPORTED_FORMAT');assert.match(result.structuredContent.error.message,hint);
      assert.ok(!result.content.some(c=>c.type==='resource'));
    }
  }
  assert.equal((await f.call('read_text_file',{root:'work',path:'raw.rtf'})).structuredContent.error.code,'UNSUPPORTED_FORMAT');
  assert.equal((await f.call('fetch',{id:'work:raw.rtf'})).structuredContent.error.code,'UNSUPPORTED_FORMAT');
});
test('all-root cursors follow configuration order and reject a removed cursor root',async t=>{
  const f=await fixture(t);
  await Promise.all([f.work,f.cowork].flatMap((dir,index)=>Array.from({length:index?4:6},(_,i)=>writeFile(path.join(dir,`ordered-${i}.txt`),'ordered'))));
  const args={root:'all',query:'ordered',limit:3};let first;const results=[];
  for(let i=0;i<10;i++) {
    const page=(await f.call('search_files',args)).structuredContent;
    results.push(...page.results);
    if(!first)first=page.nextCursor;
    if(!page.nextCursor)break;args.cursor=page.nextCursor;
  }
  assert.equal(results.length,10);assert.equal(new Set(results.map(r=>r.path)).size,10);
  assert.deepEqual(results.map(r=>r.root),[...Array(6).fill('work'),...Array(4).fill('cowork')]);
  f.config.roots=f.config.roots.filter(r=>r.id!=='work');await writeFile(f.configPath,JSON.stringify(f.config));
  assert.equal((await f.call('search_files',{...args,cursor:first})).structuredContent.error.code,'INVALID_CURSOR');
});

// Small synthetic office packages: STORED ZIP, no external converter or client data.
function officeZip(parts) {
  const local=[],central=[];let offset=0;
  const entries=Array.isArray(parts)?parts:Object.entries(parts);
  for(const [filename,body,options={}] of entries) {
    const name=Buffer.from(filename),plain=Buffer.from(body),data=options.method===8?deflateRawSync(plain):plain,crc=crc32(plain);
    const flags=options.flags??(Buffer.isBuffer(filename)?0:0x800),size=options.size??plain.length,method=options.method??0;
    const header=Buffer.alloc(30);header.writeUInt32LE(0x04034b50);header.writeUInt16LE(20,4);header.writeUInt16LE(flags,6);header.writeUInt16LE(method,8);
    header.writeUInt32LE(crc,14);header.writeUInt32LE(data.length,18);header.writeUInt32LE(size,22);header.writeUInt16LE(name.length,26);
    local.push(header,name,data);
    const directory=Buffer.alloc(46);directory.writeUInt32LE(0x02014b50);directory.writeUInt16LE(20,4);directory.writeUInt16LE(20,6);directory.writeUInt16LE(flags,8);directory.writeUInt16LE(method,10);
    directory.writeUInt32LE(crc,16);directory.writeUInt32LE(data.length,20);directory.writeUInt32LE(size,24);directory.writeUInt16LE(name.length,28);directory.writeUInt32LE(offset,42);
    central.push(directory,name);offset+=header.length+name.length+data.length;
  }
  const directory=Buffer.concat(central),end=Buffer.alloc(22),count=entries.length;
  end.writeUInt32LE(0x06054b50);end.writeUInt16LE(count,8);end.writeUInt16LE(count,10);end.writeUInt32LE(directory.length,12);end.writeUInt32LE(offset,16);
  return Buffer.concat([...local,directory,end]);
}
function syntheticDocx(text='Договор 😀 &amp; условия') {
  return officeZip({
    '_rels/.rels':'<Relationships><Relationship Type="x/officeDocument" Target="word/document.xml"/></Relationships>',
    'word/_rels/document.xml.rels':'<Relationships><Relationship Type="x/comments" Target="comments.xml"/></Relationships>',
    'word/document.xml':`<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>${text}</w:t></w:r><w:ins w:author="Автор"><w:r><w:t>новое</w:t></w:r></w:ins><w:del><w:r><w:delText>старое</w:delText></w:r></w:del><w:commentRangeStart w:id="1"/><w:r><w:t>проверить</w:t></w:r><w:commentRangeEnd w:id="1"/></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Цена</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>123</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>`,
    'word/comments.xml':'<w:comments xmlns:w="w"><w:comment w:id="1" w:author="Юрист" w:date="2026-10-17T12:00:00Z"><w:p><w:r><w:t>Уточнить</w:t></w:r></w:p></w:comment></w:comments>'
  });
}
function syntheticXlsx() {
  const ns='http://schemas.openxmlformats.org/spreadsheetml/2006/main';
  return officeZip({
    '[Content_Types].xml':'<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/></Types>',
    '_rels/.rels':'<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>',
    'xl/workbook.xml':`<workbook xmlns="${ns}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Расчёт" sheetId="1" r:id="rId1"/><sheet name="Итог" sheetId="2" state="hidden" r:id="rId2"/></sheets></workbook>`,
    'xl/_rels/workbook.xml.rels':'<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>',
    'xl/sharedStrings.xml':`<sst xmlns="${ns}" count="1099511627776" uniqueCount="1099511627776"><si><t>Сумма</t></si></sst>`,
    'xl/worksheets/sheet1.xml':`<worksheet xmlns="${ns}"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row><row r="2"><c r="A2"><v>123.5</v></c></row></sheetData></worksheet>`,
    'xl/worksheets/sheet2.xml':`<worksheet xmlns="${ns}"><sheetData><row r="3"><c r="B3"><v>456</v></c></row></sheetData></worksheet>`
  });
}
test('DOCX revisions, table rows, comments and Unicode cursor chains through MCP',async t=>{
  const f=await fixture(t),name='synthetic.docx';
  await writeFile(path.join(f.work,name),syntheticDocx('Договор 😀 &amp; условия '.repeat(60)));
  const args={root:'work',path:name};
  const info=(await f.call('inspect_document',args)).structuredContent;
  assert.equal(info.paragraphs,3);assert.equal(info.tables,1);assert.equal(info.comments,1);assert.equal(info.insertions,1);assert.equal(info.deletions,1);
  const full=(await f.call('read_document',args)).structuredContent;
  assert.equal(full.unitName,'block');assert.match(full.text,/\{\+\+новое\+\+\}\{--старое--\}\{==проверить==\}\[c1\]/);
  assert.match(full.text,/\[table 1, row 1\] \| Цена \| 123 \|/);assert.match(full.text,/\[c1\] Юрист \(2026-10-17\): Уточнить/);
  const payloads=(doc)=>{
    let chars=Array.from(doc.text.slice(doc.text.indexOf('\n')+1)),pos=0;
    return doc.units.map((u,i)=>{
      if(i)pos+=2;
      if(u.comment && chars.slice(pos,pos+9).join('')==='Comments\n')pos+=9;
      const value=chars.slice(pos,pos+u.chars).join('');pos+=u.chars;return value;
    });
  };
  const expected=payloads(full),recovered=expected.map(()=>''),keys=full.units.map(u=>JSON.stringify([u.paragraph,u.table,u.row,u.comment]));
  let cursor,calls=0;
  do {
    const r=await f.call('read_document',{...args,maxChars:140,...(cursor?{cursor}:{})});assert.equal(r.isError,undefined);
    const doc=r.structuredContent,values=payloads(doc);assert.ok(Array.from(doc.text).length<=200000);
    for(let i=0;i<doc.units.length;i++){const u=doc.units[i],key=JSON.stringify([u.paragraph,u.table,u.row,u.comment]);recovered[keys.indexOf(key)]+=values[i];}
    cursor=doc.nextCursor;assert.equal(doc.truncated,Boolean(cursor));assert.ok(++calls<100);
  }while(cursor);
  assert.deepEqual(recovered,expected);
  assert.equal((await f.call('read_file',args)).structuredContent.text,full.text);
  assert.equal((await f.call('fetch',{id:`work:${name}`})).structuredContent.text,full.text);
  const first=(await f.call('read_document',{...args,maxChars:100})).structuredContent;
  await writeFile(path.join(f.work,name),syntheticDocx('изменён'));
  assert.equal((await f.call('read_document',{...args,cursor:first.nextCursor})).structuredContent.error.code,'STALE_CURSOR');
});
test('XLSX cached rows, sheet selection and cursor request identity through MCP',async t=>{
  const f=await fixture(t),args={root:'work',path:'synthetic.xlsx'};await writeFile(path.join(f.work,args.path),syntheticXlsx());
  const schema=(await f.client.listTools()).tools.find(t=>t.name==='read_document').inputSchema;
  assert.equal(schema.properties.sheet.type,'string');assert.equal(schema.properties.sheet.maxLength,256);
  const inspection=await f.call('inspect_document',args);assert.equal(inspection.isError,undefined,JSON.stringify(inspection.structuredContent));
  const info=inspection.structuredContent;assert.deepEqual(info.sheets.map(s=>[s.name,s.rows,s.columns,s.visible]),[['Расчёт',2,1,true],['Итог',1,1,false]]);
  const all=(await f.call('read_file',args)).structuredContent;assert.equal(all.format,'xlsx');assert.equal(all.unitName,'row');assert.deepEqual(all.units.map(u=>[u.sheet,u.row]),[['Расчёт',1],['Расчёт',2],['Итог',3]]);assert.match(all.text,/2\t123.5/);
  for(const sheet of ['2','Итог']) {const selected=(await f.call('read_document',{...args,sheet})).structuredContent;assert.deepEqual(selected.units.map(u=>[u.sheet,u.row]),[['Итог',3]]);assert.match(selected.text,/hidden/);assert.match(selected.text,/3\t456/);}
  const first=(await f.call('read_document',{...args,sheet:'Расчёт',maxChars:1})).structuredContent;assert.ok(first.nextCursor);
  assert.equal((await f.call('read_document',{...args,sheet:'Итог',cursor:first.nextCursor})).structuredContent.error.code,'INVALID_CURSOR');
  const next=await f.call('read_document',{...args,sheet:'Расчёт',cursor:first.nextCursor,maxChars:200000});assert.equal(next.isError,undefined);assert.ok(next.structuredContent.units.every(u=>u.sheet==='Расчёт'));
  assert.equal((await f.call('read_document',{...args,sheet:'missing'})).structuredContent.error.code,'INVALID_ARGUMENT');
  assert.match((await f.call('fetch',{id:'work:synthetic.xlsx'})).structuredContent.text,/Сумма/);
});
test('EML headers, body, attachment provenance and document search',async t=>{
  const f=await fixture(t),args={root:'work',path:'synthetic.eml'};
  const raw='Subject: =?windows-1251?Q?=CF=F0=E8=E2=E5=F2?=\r\nFrom: Sender <sender@example.test>\r\nTo: Reader <reader@example.test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nEML_ONLY_NEEDLE Текст 😀\r\n\r\nВторой абзац\r\n--m\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=synthetic.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nAQID\r\n--m--\r\n';
  await writeFile(path.join(f.work,args.path),raw);
  const info=(await f.call('inspect_document',args)).structuredContent;assert.equal(info.subject,'Привет');assert.equal(info.bodySource,'text/plain');assert.equal(info.attachments[0].size,3);assert.equal(info.attachments[0].path,'work/synthetic.eml::1');
  const doc=(await f.call('read_document',args)).structuredContent;assert.equal(doc.unitName,'part');assert.ok(doc.units.some(u=>u.part==='body'));assert.match(doc.text,/Второй абзац/);assert.match(doc.text,/synthetic.pdf/);
  assert.equal((await f.call('read_file',args)).structuredContent.text,doc.text);assert.equal((await f.call('fetch',{id:'work:synthetic.eml'})).structuredContent.text,doc.text);
  assert.equal((await f.call('search_text',{root:'work',query:'EML_ONLY_NEEDLE'})).structuredContent.results.length,1);
  assert.equal((await f.call('get_file_info',args)).structuredContent.format,'eml');assert.ok(!JSON.stringify(info).includes(f.work));
});
test('EML multipart selections survive complete cursor chains without alternative duplicates',async t=>{
  const f=await fixture(t);
  const first='Первая часть 😀 '.repeat(20),second='Последняя часть 📝 '.repeat(20);
  const mixed=`Subject: mixed\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n${first}\r\n--m\r\nContent-Type: image/png\r\nContent-ID: <pixel>\r\nContent-Disposition: inline; filename=pixel.png\r\nContent-Transfer-Encoding: base64\r\n\r\nU0VDUkVUX0FUVEFDSE1FTlQ=\r\n--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n${second}\r\n--m--\r\n`;
  const alternative='MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=a\r\n\r\n--a\r\nContent-Type: text/plain\r\n\r\nSAME_CONTENT\r\n--a\r\nContent-Type: text/html\r\n\r\n<p>SAME_CONTENT</p>\r\n--a--\r\n';
  for(const [name,raw,expected] of [['mixed.eml',mixed,[first,second]],['alternative.eml',alternative,['SAME_CONTENT']]]) {
    const args={root:'work',path:name};await writeFile(path.join(f.work,name),raw);
    const full=(await f.call('read_document',args)).structuredContent;
    assert.deepEqual(full.units.filter(u=>u.part==='body').map(u=>u.paragraph),expected.map((_,i)=>i+1));
    const recovered=expected.map(()=>''),headings={headers:'Mail headers\n',body:'Mail body · text/plain\n',attachments:'Attachments (open readable items with ::N)\n'};
    let cursor,calls=0;
    do {
      const r=await f.call('read_document',{...args,maxChars:80,...(cursor?{cursor}:{})});assert.equal(r.isError,undefined);
      const doc=r.structuredContent,chars=Array.from(doc.text.slice(doc.text.indexOf('\n')+1));let pos=0;
      for(let i=0;i<doc.units.length;i++) {
        const u=doc.units[i];if(i)pos+=2;
        const heading=Array.from(headings[u.part]||'');
        if(chars.slice(pos,pos+heading.length).join('')===heading.join(''))pos+=heading.length;
        const value=chars.slice(pos,pos+u.chars).join('');pos+=u.chars;
        if(u.part==='body')recovered[u.paragraph-1]+=value;
      }
      assert.equal(pos,chars.length);cursor=doc.nextCursor;assert.equal(doc.truncated,Boolean(cursor));assert.ok(++calls<100);
    }while(cursor);
    assert.ok(calls>1);assert.deepEqual(recovered.map(s=>s.trim()),expected.map(s=>s.trim()));
    assert.ok(!recovered.join('').includes('SECRET_ATTACHMENT'));
    const info=(await f.call('inspect_document',args)).structuredContent;
    assert.equal(info.attachments.length,name==='mixed.eml'?1:0);
    if(name==='mixed.eml'){assert.equal(info.attachments[0].inline,true);assert.equal(info.attachments[0].path,'work/mixed.eml::1');}
    else assert.equal(recovered.join('').split('SAME_CONTENT').length-1,1);
  }
});
test('office and mail on-disk limits fail before parsing without disclosing host paths',async t=>{
  const f=await fixture(t);
  for(const [ext,mib] of [['docx',256],['xlsx',64],['xls',64],['eml',64],['msg',256]]) {
    const name=`oversized.${ext}`,file=await open(path.join(f.work,name),'w');await file.truncate(mib*1024*1024+1);await file.close();
    for(const tool of ['read_document','inspect_document','read_file']) {const result=await f.call(tool,{root:'work',path:name});assert.equal(result.structuredContent.error.code,'FILE_TOO_LARGE',`${tool} ${ext}`);assert.ok(!JSON.stringify(result).includes(f.work));}
  }
});
test('truncated office containers fail explicitly and the MCP server remains usable',async t=>{
  const f=await fixture(t);
  for(const ext of ['docx','xlsx','xls','msg']) {
    await writeFile(path.join(f.work,`broken.${ext}`),Buffer.from([0x50,0x4b,3,4]));
    const result=await f.call('read_document',{root:'work',path:`broken.${ext}`});
    assert.equal(result.isError,true);assert.equal(result.structuredContent.error.code,'DOCUMENT_INVALID');assert.ok(!JSON.stringify(result).includes(f.work));
  }
  assert.equal((await f.call('read_document',{root:'work',path:'Привет.md'})).structuredContent.format,'text');
});

test('Ф5 ZIP member paths, safe listings, nested readers and cursor fit',async t=>{
  const f=await fixture(t);
  const inner=officeZip({'x.txt':'NESTED_NEEDLE','deeper.zip':officeZip({'last.zip':officeZip({'end.txt':'end'})})});
  await writeFile(path.join(f.work,'a.zip'),officeZip({'folder/doc.docx':syntheticDocx(),'doc.pdf':syntheticPdf(),'inner.zip':inner,'.env':'SECRET','../outside.txt':'NEVER','file.txt':'member text'}));
  const listed=(await f.call('list_directory',{root:'work',path:'a.zip',depth:2})).structuredContent;
  assert.ok(listed.entries.some(e=>e.path==='work/a.zip::folder/doc.docx'));
  assert.ok(!listed.entries.some(e=>e.name==='.env'));
  assert.equal(listed.entries.find(e=>e.name==='../outside.txt')?.readable,false);
  for(const [member,pattern] of [['doc.pdf',/page 1/],['folder/doc.docx',/Договор/],['inner.zip::x.txt',/NESTED_NEEDLE/]]) {
    const read=await f.call('read_document',{root:'work',path:`a.zip::${member}`});assert.equal(read.isError,undefined);assert.match(read.structuredContent.text,pattern);
  }
  const prefix=(await f.call('list_directory',{root:'work',path:'a.zip::folder'})).structuredContent;
  assert.deepEqual(prefix.entries.map(e=>e.name),['doc.docx']);
  assert.equal((await f.call('read_document',{root:'work',path:'a.zip::inner.zip::deeper.zip::last.zip::end.txt'})).structuredContent.error.code,'NESTING_TOO_DEEP');
  for(const member of ['../outside.txt','.env','missing.txt']) {
    const r=await f.call('read_document',{root:'work',path:`a.zip::${member}`});assert.equal(r.isError,true);assert.ok(!JSON.stringify(r).includes(f.work));
  }
  const summary=(await f.call('read_document',{root:'work',path:'a.zip'})).structuredContent;assert.equal(summary.kind,'inspect');assert.ok(summary.entries>0);
  await writeFile(path.join(f.work,'long.zip'),officeZip({'a.txt':'a',[`${'d'.repeat(1600)}/inner.txt`]:'hidden',[`${'x'.repeat(1600)}.txt`]:'hidden','y.txt':'y','z.txt':'z'}));
  for(const limit of [100,1]) {
    const names=[];let cursor,skipped=0;
    do {
      const r=await f.call('list_directory',{root:'work',path:'long.zip',depth:2,limit,...(cursor?{cursor}:{})});assert.equal(r.isError,undefined);
      names.push(...r.structuredContent.entries.map(e=>e.name));skipped+=r.structuredContent.skippedTooDeep;cursor=r.structuredContent.nextCursor;
      if(cursor)assert.ok(cursor.length<=2048);
    }while(cursor);
    assert.deepEqual(names,['a.txt','y.txt','z.txt']);assert.equal(skipped,2);
  }
});

test('Ф5 search inside documents, single-file hits and mixed folder continuation',async t=>{
  const f=await fixture(t);await mkdir(path.join(f.work,'mixed'));
  for(let i=0;i<23;i++)await writeFile(path.join(f.work,'mixed',`${String(i).padStart(2,'0')}.${i%2?'docx':'txt'}`),i%2?syntheticDocx('PHASEFIVE_NEEDLE'): 'PHASEFIVE_NEEDLE');
  let cursor;const hits=[];
  do {const r=(await f.call('search_text',{root:'work',path:'mixed',query:'PHASEFIVE_NEEDLE',limit:5,...(cursor?{cursor}:{})})).structuredContent;hits.push(...r.results);cursor=r.nextCursor;}while(cursor);
  assert.equal(hits.length,23);assert.equal(new Set(hits.map(h=>h.path)).size,23);assert.ok(hits.some(h=>h.locator?.paragraph));
  await writeFile(path.join(f.work,'hits.txt'),'NEEDLE here NEEDLE\nthen NEEDLE');
  const all=[];cursor=undefined;
  do {const r=await f.call('search_text',{root:'work',path:'hits.txt',query:'NEEDLE',limit:1,...(cursor?{cursor}:{})});assert.equal(r.isError,undefined);all.push(...r.structuredContent.results);cursor=r.structuredContent.nextCursor;}while(cursor);
  assert.equal(all.length,3);assert.deepEqual(all.map(h=>h.locator.line),[1,1,2]);
});

function mailAttachment(data,name,mime='application/octet-stream') {
  return `Subject: synthetic attachment\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nparent body\r\n--m\r\nContent-Type: ${mime}\r\nContent-Disposition: attachment; filename="${name}"\r\nContent-Transfer-Encoding: base64\r\n\r\n${Buffer.from(data).toString('base64')}\r\n--m--\r\n`;
}
test('Ф5 ZIP CP866, compression and corrupted member bounds through MCP',async t=>{
  const f=await fixture(t),cp866=Buffer.from([0x88,0xe1,0xaa,46,112,100,102]);
  await writeFile(path.join(f.work,'cp.zip'),officeZip([[cp866,syntheticPdf()],['text.txt','compressed text',{method:8}],['enc.txt','secret',{flags:1}],['unsupported.txt','data',{method:12}]]));
  const list=(await f.call('list_directory',{root:'work',path:'cp.zip'})).structuredContent.entries;
  assert.ok(list.some(e=>e.name==='Иск.pdf'));assert.equal(list.find(e=>e.name==='enc.txt').reason,'ENCRYPTED_MEMBER');assert.equal(list.find(e=>e.name==='unsupported.txt').reason,'UNSUPPORTED_COMPRESSION');
  assert.match((await f.call('read_document',{root:'work',path:'cp.zip::Иск.pdf'})).structuredContent.text,/Synthetic native PDF/);
  assert.equal((await f.call('read_text_file',{root:'work',path:'cp.zip::text.txt'})).structuredContent.text,'compressed text');
  for(const [name,options,code] of [['bomb.zip',{size:2*1024*1024},'ZIP_BOMB'],['wrong.zip',{size:2},'ZIP_INVALID'],['big.zip',{size:256*1024*1024+1},'FILE_TOO_LARGE']]) {
    await writeFile(path.join(f.work,name),officeZip([['x.txt','hello',options]]));
    assert.equal((await f.call('read_document',{root:'work',path:`${name}::x.txt`})).structuredContent.error.code,code);
  }
  for(const [member,code] of [['enc.txt','ENCRYPTED_MEMBER'],['unsupported.txt','UNSUPPORTED_COMPRESSION']])assert.equal((await f.call('read_file',{root:'work',path:`cp.zip::${member}`})).structuredContent.error.code,code);
});
test('Ф5 attachments share readers, views, search and stable member cursor identity',async t=>{
  const f=await fixture(t);
  for(const [name,data,pattern] of [['text.txt','first\nsecond',/first/],['doc.docx',syntheticDocx(),/Договор/],['page.pdf',syntheticPdf(),/Synthetic native PDF/],['book.xlsx',syntheticXlsx(),/Сумма/]]) {
    await writeFile(path.join(f.work,'letter.eml'),mailAttachment(data,name));
    const args={root:'work',path:'letter.eml::1'};
    assert.match((await f.call('read_document',args)).structuredContent.text,pattern);
    assert.match((await f.call('read_file',args)).structuredContent.text,pattern);
    const fetched=(await f.call('fetch',{id:'work:letter.eml::1'})).structuredContent;assert.match(fetched.text,pattern);
    if(name==='page.pdf')assert.equal(fetched.metadata.mimeType,'application/pdf');
    assert.equal((await f.call('list_directory',{root:'work',path:'letter.eml'})).structuredContent.entries[0].index,1);
  }
  await writeFile(path.join(f.work,'letter.eml'),mailAttachment(syntheticPng(64,32),'image.png'));
  const image=await f.call('read_file',{root:'work',path:'letter.eml::1',region:{x:0,y:0,width:0.5,height:1}});assert.equal(image.structuredContent.kind,'image');assert.equal(image.structuredContent.width,32);
  await writeFile(path.join(f.work,'letter.eml'),mailAttachment('Subject: nested\r\n\r\nnested body','nested.bin','message/rfc822'));
  const nested=(await f.call('read_document',{root:'work',path:'letter.eml::1'})).structuredContent;assert.equal(nested.format,'eml');assert.match(nested.text,/nested body/);
  await writeFile(path.join(f.work,'letter.eml'),mailAttachment('SECRET','.env'));assert.deepEqual((await f.call('list_directory',{root:'work',path:'letter.eml'})).structuredContent.entries,[]);assert.equal((await f.call('read_file',{root:'work',path:'letter.eml::1'})).structuredContent.error.code,'BLOCKED_PATH');
  await writeFile(path.join(f.work,'identity.zip'),officeZip({'a.txt':'needle '.repeat(30),'b.txt':'needle '.repeat(30)}));
  const args={root:'work',path:'identity.zip::a.txt'};
  const doc=(await f.call('read_document',{...args,maxChars:100})).structuredContent;
  assert.ok(doc.nextCursor);
  assert.equal((await f.call('read_document',{...args,path:'identity.zip::b.txt',cursor:doc.nextCursor})).structuredContent.error.code,'INVALID_CURSOR');
  const found=(await f.call('search_text',{...args,query:'needle',limit:1})).structuredContent;assert.ok(found.nextCursor);
  assert.equal((await f.call('search_text',{...args,path:'identity.zip::b.txt',query:'needle',cursor:found.nextCursor})).structuredContent.error.code,'INVALID_CURSOR');
  await writeFile(path.join(f.work,'identity.zip'),officeZip({'a.txt':'changed needle text'}));
  assert.equal((await f.call('read_document',{...args,cursor:doc.nextCursor})).structuredContent.error.code,'STALE_CURSOR');
  assert.equal((await f.call('search_text',{...args,query:'needle',cursor:found.nextCursor})).structuredContent.error.code,'STALE_CURSOR');
});
test('Ф5 PDF all hits carry page numbers and folder hits reach connector search',async t=>{
  const f=await fixture(t);await writeFile(path.join(f.work,'document.pdf'),syntheticPdf('UNIQUEPDFNEEDLE first UNIQUEPDFNEEDLE second UNIQUEPDFNEEDLE'));
  let cursor;const hits=[];
  do {const r=await f.call('search_text',{root:'work',path:'document.pdf',query:'UNIQUEPDFNEEDLE',limit:2,...(cursor?{cursor}:{})});assert.equal(r.isError,undefined);hits.push(...r.structuredContent.results);cursor=r.structuredContent.nextCursor;}while(cursor);
  assert.equal(hits.length,3);assert.ok(hits.every(h=>h.locator.page===1 && h.snippet.startsWith('[p. 1]')));
  const folder=(await f.call('search_text',{root:'work',query:'UNIQUEPDFNEEDLE'})).structuredContent;assert.equal(folder.results.length,1);assert.equal(folder.documentsSearched,1);
  assert.equal((await f.call('search',{query:'UNIQUEPDFNEEDLE'})).structuredContent.results[0].id,'work:document.pdf');
});

test('Ф5-F1-B1 container depth cap on ZIP and EML listings and inspection',async t=>{
  const f=await fixture(t);
  for(const [name,bytes] of [['c.zip',officeZip({'c.txt':'SAFE CHILD','d.zip':officeZip({'d.txt':'depth four'})})],['letter.eml',mailAttachment('SAFE CHILD','child.txt')]]) {
    const raw=`a.zip::nested.zip::b.zip::${name}`;
    await writeFile(path.join(f.work,'a.zip'),officeZip({'nested.zip':officeZip({'b.zip':officeZip({[name]:bytes})})}));
    const parent='a.zip::nested.zip::b.zip';
    const list=await f.call('list_directory',{root:'work',path:parent});assert.equal(list.isError,undefined);assert.equal(list.structuredContent.entries[0].readable,true);
    const parentInfo=await f.call('inspect_document',{root:'work',path:parent});assert.equal(parentInfo.isError,undefined);assert.equal(parentInfo.structuredContent.entries,1);
    for(const tool of ['list_directory','inspect_document']) {
      const result=await f.call(tool,{root:'work',path:raw});assert.equal(result.isError,true,`${tool}: ${name}`);assert.equal(result.structuredContent.error.code,'NESTING_TOO_DEEP');
      assert.equal(result.structuredContent.entries,undefined);assert.equal(result.structuredContent.attachments,undefined);
      assert.ok(!JSON.stringify(result).includes(`${raw}::`));
    }
    const reader=await f.call('read_document',{root:'work',path:raw});
    if(name.endsWith('.eml')) {
      assert.equal(reader.isError,undefined);assert.match(reader.structuredContent.text,/parent body/);assert.equal(reader.structuredContent.attachments[0].path,undefined);
    } else {assert.equal(reader.structuredContent.error.code,'NESTING_TOO_DEEP');}
  }
  for(const [name,bytes,child] of [['c.zip',officeZip({'c.txt':'SAFE CHILD'}),'c.txt'],['letter.eml',mailAttachment('SAFE CHILD','child.txt'),'1']]) {
    await writeFile(path.join(f.work,'a.zip'),officeZip({'nested.zip':officeZip({[name]:bytes})}));
    const raw=`a.zip::nested.zip::${name}`;
    const list=await f.call('list_directory',{root:'work',path:raw});assert.equal(list.isError,undefined);assert.equal(list.structuredContent.entries[0].readable,true);
    const info=await f.call('inspect_document',{root:'work',path:raw});assert.equal(info.isError,undefined);
    const read=await f.call('read_document',{root:'work',path:`${raw}::${child}`});assert.equal(read.isError,undefined);assert.match(read.structuredContent.text,/SAFE CHILD/);
  }
});
