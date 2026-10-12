// Comparable end-to-end MCP benchmarks. Synthetic local files; no user documents.
import { mkdtemp,mkdir,writeFile,rm } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';
import { performance } from 'node:perf_hooks';
import assert from 'node:assert/strict';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';

const base=await mkdtemp(path.join(os.tmpdir(),'local-files-bench-'));
const work=path.join(base,'work'),cowork=path.join(base,'cowork');
await mkdir(work);await mkdir(cowork);
const body=('Договор поставки. Стороны согласовали оплату и передачу товара.\n'.repeat(1400)).slice(0,64000);
const bytes=Buffer.byteLength(body);const count=256;
for(let i=0;i<count;i++)await writeFile(path.join(work,`document-${String(i).padStart(4,'0')}.md`),body);
await writeFile(path.join(cowork,'needle.md'),'Уникальная искомая фраза\n');
const config=path.join(base,'folders.json');
await writeFile(config,JSON.stringify({version:1,roots:[{id:'work',path:work},{id:'cowork',path:cowork}],next_root_id:1}));
const binaries={node:{command:process.execPath,args:[path.resolve('src/server.js')],env:{...process.env,MCP_WORK_ROOT:work,MCP_COWORK_ROOT:cowork}},rust:{command:path.resolve('target/release/local-files'),args:['serve','--config',config]}};
const median=values=>{const s=[...values].sort((a,b)=>a-b);return (s[Math.floor((s.length-1)/2)]+s[Math.floor(s.length/2)])/2;};
const rss=pid=>Number(execFileSync('/bin/ps',['-p',String(pid),'-o','rss='],{encoding:'utf8'}).trim())/1024;
const metrics={};
try{
  // Alternate order to reduce systematic warm-cache bias.
  for(let round=0;round<4;round++)for(const which of round%2?['rust','node']:['node','rust']){
    const start=performance.now();
    const transport=new StdioClientTransport({...binaries[which],stderr:'pipe'});
    transport.stderr?.on('data',()=>{});
    const client=new Client({name:'benchmark',version:'1'});
    try{
      await client.connect(transport);
      const sample={startup_ms:performance.now()-start,idle_rss_mib:rss(transport.pid),search_ms:[],read_ms:[]};
      for(let i=0;i<5;i++){
        let then=performance.now();
        const found=await client.callTool({name:'search_text',arguments:{root:'all',query:'ФРАЗА-КОТОРОЙ-НЕ-СУЩЕСТВУЕТ',limit:100}});
        sample.search_ms.push(performance.now()-then);
        assert.equal(found.isError,undefined);assert.equal(found.structuredContent.results.length,0);assert.equal(found.structuredContent.truncated,false);
        then=performance.now();
        const read=await client.callTool({name:'read_text_file',arguments:{root:'work',path:'document-0000.md'}});
        sample.read_ms.push(performance.now()-then);assert.equal(read.structuredContent.text,body);
      }
      sample.after_work_rss_mib=rss(transport.pid);
      (metrics[which]??=[]).push(sample);
    }finally{await client.close();}
  }
  const summary=Object.fromEntries(Object.entries(metrics).map(([name,rows])=>[name,{
    startup_ms:median(rows.map(r=>r.startup_ms)),idle_rss_mib:median(rows.map(r=>r.idle_rss_mib)),
    full_scan_ms:median(rows.flatMap(r=>r.search_ms)),read_ms:median(rows.flatMap(r=>r.read_ms)),
    after_work_rss_mib:median(rows.map(r=>r.after_work_rss_mib))
  }]));
  const report={date:new Date().toISOString(),platform:process.platform,arch:process.arch,node:process.version,
    dataset:{files:count+1,total_bytes:bytes*count+Buffer.byteLength('Уникальная искомая фраза\n'),query:'case-insensitive Cyrillic miss, full scan'},
    method:'4 fresh processes per implementation, 5 scans and reads each, alternating order, warm OS cache, median; RSS excludes tunnel and tray',summary,raw:metrics};
  await mkdir('artifacts',{recursive:true});await writeFile('artifacts/benchmark.json',JSON.stringify(report,null,2)+'\n');
  console.log(JSON.stringify(report,null,2));
}finally{await rm(base,{recursive:true,force:true});}
