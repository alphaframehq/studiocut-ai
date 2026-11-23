import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { stripTypeScriptTypes } from 'node:module';
const temp=fs.mkdtempSync(path.join(os.tmpdir(),'studiocut-director-'));
try {
  for (const name of ['project','timeline','analysis','edl','effects','finishing']) {
    const source=fs.readFileSync(new URL(`../../src/${name}.ts`,import.meta.url),'utf8');
    fs.writeFileSync(path.join(temp,`${name}.mjs`),stripTypeScriptTypes(source).replace(/'\.\/(project|timeline|analysis|edl|finishing)'/g,"'./$1.mjs'"));
  }
  const {compileEdl}=await import(pathToFileURL(path.join(temp,'edl.mjs')));
  const {travel,remember,emptyHistory}=await import(pathToFileURL(path.join(temp,'timeline.mjs')));
  const {gainAt,zoomAt,applyLut}=await import(pathToFileURL(path.join(temp,'effects.mjs')));
  const {project,cases}=JSON.parse(fs.readFileSync(process.argv[2],'utf8'));
  let passed=0;
  for (const test of cases) {
    if(test.valid) { const next=compileEdl(project,test.edl);assert.equal(next.revision,project.revision+1);assert.equal(next.schemaVersion,5);const undo=travel(next,remember(emptyHistory(),project.tracks),'undo');assert.deepEqual(undo.project.tracks,project.tracks);assert.deepEqual(travel(undo.project,undo.history,'redo').project.tracks,next.tracks); }
    else assert.throws(()=>compileEdl(project,test.edl),JSON.stringify(test.edl));
    passed++;
  }
  const {defaultFinishing,validateFinishing,validateCube}=await import(pathToFileURL(path.join(temp,'finishing.mjs')));
  const {validateProject}=await import(pathToFileURL(path.join(temp,'project.mjs')));
  const {sourceOffset}=await import(pathToFileURL(path.join(temp,'timeline.mjs')));
  const finishing=defaultFinishing();validateFinishing(finishing,'video',300);assert.throws(()=>validateFinishing({...finishing,speed:NaN},'video',300));
  assert.throws(()=>validateCube('LUT_3D_SIZE 2\nNaN 0 0'));
  const graded=structuredClone(project);graded.schemaVersion=6;graded.tracks[0].clips[0].effects={zoom:[],lut:'warm',duck:[],finishing};validateProject(graded);assert.equal(compileEdl(graded,cases[0].edl).schemaVersion,6);
  assert.equal(sourceOffset({...graded.tracks[0].clips[0],effects:{...graded.tracks[0].clips[0].effects,finishing:{...finishing,speed:2}}},3),6);
  assert.throws(()=>sourceOffset({...graded.tracks[0].clips[0],effects:{...graded.tracks[0].clips[0].effects,finishing:{...finishing,speed:1.5}}},3));
  const plan=cases[0].edl;
  assert.throws(()=>compileEdl({...project,revision:project.revision+1},plan));
  const locked=structuredClone(project);locked.tracks[1].locked=true;assert.throws(()=>compileEdl(locked,plan));
  assert.equal(gainAt([{frame:0,gain:0},{frame:30,gain:1}],15),0.5);
  assert.equal(zoomAt([{frame:0,scale:1,x:0.5,y:0.5},{frame:30,scale:3,x:0.5,y:0.5}],15).scale,2);
  const pixels=new Uint8ClampedArray([123,54,216,255,0,255,128,255]);const expected=pixels.slice();applyLut(pixels,'identity');assert.deepEqual(pixels,expected);applyLut(pixels,'warm');assert.notDeepEqual(pixels,expected);
  console.log(`Director deterministic contract eval: ${passed}/${cases.length} TypeScript cases agree with Rust; stale/locked rejection, zoom/gain interpolation and real 3D LUT processing passed. This is not a model-quality evaluation.`);
} finally {fs.rmSync(temp,{recursive:true,force:true});}
