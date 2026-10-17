import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, symlink, rm, rename } from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import test from 'node:test';
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
  assert.deepEqual(tools.map(t=>t.name).sort(),['fetch','get_file_info','list_directory','list_roots','read_file','read_text_file','search','search_files','search_text']);
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
  await writeFile(path.join(f.work,'sample.pdf'),Buffer.from([0xff,0x00,0x01]));
  const binary=await f.call('read_file',{root:'work',path:'sample.pdf'});
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
  await writeFile(path.join(f.work,'big.txt'),Buffer.alloc(5*1024*1024+1,65));
  assert.equal((await f.call('read_text_file',{root:'work',path:'big.txt'})).structuredContent.error.code,'FILE_TOO_LARGE');
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
