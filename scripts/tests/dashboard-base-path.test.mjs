import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
const html=readFileSync(new URL('../../src/web_dashboard.html',import.meta.url),'utf8');
const start=html.indexOf('  var dashboardBase =');
const end=html.indexOf('  /** ...scoped to the repo',start);
const functions=html.slice(start,end);
test('real dashboard URL helpers preserve roots and mount API/auth/media paths once',()=>{
 for(const pathname of ['/','/storyhook/']){
  const context={window:{location:{pathname}}};vm.createContext(context);vm.runInContext(functions,context);
  const base=pathname.slice(0,-1);
  for(const path of ['/api/repos','/api/events','/api/preferences','/token','/handoff']) assert.equal(context.dashboardPath(path),base+path);
  assert.equal(context.repoApiBase('two words'),base+'/api/repos/two%20words');
  assert.equal(context.dashboardPath(context.repoApiBase('demo')+'/data'),base+'/api/repos/demo/data');
  assert.equal(context.dashboardPath('https://elsewhere/image.png'),'https://elsewhere/image.png');
 }
});
test('all XHR and SSE browser entry points use the mount-aware helper',()=>{
 const opens=[...html.matchAll(/xhr\.open\([^\n]+/g)].map(x=>x[0]);
 assert.ok(opens.length>=5);
 assert.ok(opens.every(line=>line.includes('dashboardPath(')),opens.join('\n'));
 assert.ok(html.includes('new EventSource(dashboardPath("/api/events"))'));
 assert.ok(html.includes('return repoApiBase(repoId) + "/story/"'));
});
