import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, symlink, rm, rename, open } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import test from 'node:test';
import { deflateSync } from 'node:zlib';
import { execFileSync } from 'node:child_process';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';

const binary=path.resolve(process.env.NATIVE_BINARY || 'target/release/local-files');
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
  // Passthrough is extension-based, including when the bytes happen to be valid UTF-8.
  for(const [ext,mimeType] of [['PNG','image/png'],['jpg','image/jpeg'],['jpeg','image/jpeg'],['gif','image/gif'],['webp','image/webp']]) {
    await writeFile(path.join(f.work,`probe.${ext}`),'GIF89a');
    const read=await f.call('read_file',{root:'work',path:`probe.${ext}`});
    assert.deepEqual(read.content.find(c=>c.type==='image'),{type:'image',data:Buffer.from('GIF89a').toString('base64'),mimeType});
  }
  await writeFile(path.join(f.work,'oversized.png'),Buffer.alloc(15*1024*1024+1));
  assert.equal((await f.call('read_file',{root:'work',path:'oversized.png'})).structuredContent.error.code,'FILE_TOO_LARGE');
  assert.equal((await f.call('read_text_file',{root:'work',path:'pixel.png'})).structuredContent.error.code,'NOT_TEXT');
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

function syntheticPdf() {
  const image=deflateSync(Buffer.alloc(1000000,200));
  const text=Buffer.from('BT /F 12 Tf (Synthetic native PDF text for the first page.) Tj ET');
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
