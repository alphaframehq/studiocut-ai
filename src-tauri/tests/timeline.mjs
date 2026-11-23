import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { stripTypeScriptTypes } from 'node:module';

const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'studiocut-contract-'));
try {
  for (const name of ['project', 'timeline', 'analysis', 'edl', 'finishing']) {
    const source = fs.readFileSync(new URL(`../../src/${name}.ts`, import.meta.url), 'utf8');
    const output = stripTypeScriptTypes(source).replace(/'\.\/(project|timeline|analysis|edl|finishing)'/g, "'./$1.mjs'");
    fs.writeFileSync(path.join(temp, `${name}.mjs`), output);
  }
  const { validateProject } = await import(pathToFileURL(path.join(temp, 'project.mjs')));
  const { editTimeline, remember, travel, emptyHistory, audible } = await import(pathToFileURL(path.join(temp, 'timeline.mjs')));
  let project = validateProject(JSON.parse(fs.readFileSync(process.argv[2], 'utf8')));
  const assetId = project.assets[0].id;
  const initial = structuredClone(project);
  project = editTimeline(project, { type: 'addClip', assetId, start: 0 });
  assert.deepEqual(initial.tracks, []);
  assert.equal(project.tracks.length, 2);
  assert.equal(project.tracks[0].clips[0].linkId, project.tracks[1].clips[0].linkId);
  let clipId = project.tracks[0].clips[0].id;
  project = editTimeline(project, { type: 'trim', clipId, edge: 'in', frame: 15 });
  assert.equal(project.tracks[1].clips[0].sourceIn, 15);
  project = editTimeline(project, { type: 'trim', clipId, edge: 'out', frame: 150 });
  project = editTimeline(project, { type: 'split', clipId, frame: 60 });
  assert.equal(project.tracks[0].clips[0].duration, 45);
  assert.equal(project.tracks[1].clips[1].sourceIn, 60);
  assert.notEqual(project.tracks[0].clips[0].linkId, project.tracks[0].clips[1].linkId);
  assert.equal(project.tracks[0].clips[1].linkId, project.tracks[1].clips[1].linkId);
  assert.throws(() => editTimeline(project, { type: 'split', clipId, frame: 15 }), /inside/);
  assert.throws(() => editTimeline(project, { type: 'move', clipId, start: 30 }), /Invalid clip/);
  assert.throws(() => editTimeline(project, { type: 'trim', clipId, edge: 'in', frame: -1 }), /Invalid clip/);
  assert.throws(() => editTimeline(project, { type: 'trim', clipId, edge: 'out', frame: 500 }), /Invalid clip/);
  const locked = editTimeline(project, { type: 'toggle', trackId: project.tracks[1].id, flag: 'locked' });
  assert.throws(() => editTimeline(locked, { type: 'remove', clipId }), /Unlock/);
  assert.throws(() => editTimeline(locked, { type: 'split', clipId, frame: 30 }), /Unlock/);
  const muted = editTimeline(project, { type: 'toggle', trackId: project.tracks[1].id, flag: 'muted' });
  assert.equal(audible(muted.tracks[1], muted.tracks), false);
  let solo = editTimeline(project, { type: 'addTrack', kind: 'audio' });
  solo = editTimeline(solo, { type: 'toggle', trackId: solo.tracks[2].id, flag: 'solo' });
  assert.equal(audible(solo.tracks[1], solo.tracks), false);
  assert.equal(audible(solo.tracks[0], solo.tracks), true);
  for (const mutate of [
    p => { p.tracks[1].clips[0].duration++; },
    p => { p.tracks[0].clips[0].assetId = crypto.randomUUID(); },
    p => { p.tracks[0].clips[0].start = 1.5; },
    p => { p.tracks[0].clips[0].duration = 0; },
    p => { p.tracks[0].clips[0].unknown = true; },
    p => { p.schemaVersion = 2; },
  ]) { const p = structuredClone(project); mutate(p); assert.throws(() => validateProject(p)); }
  const removed = editTimeline(project, { type: 'remove', clipId });
  assert.equal(removed.tracks[0].clips.length, 1);
  assert.equal(removed.tracks[1].clips.length, 1);
  let history = remember(emptyHistory(), project.tracks);
  const undo = travel(removed, history, 'undo');
  assert.deepEqual(undo.project.tracks, project.tracks);
  assert.deepEqual(travel(undo.project, undo.history, 'redo').project.tracks, removed.tracks);
  for (let i = 0; i < 60; i++) history = remember(history, project.tracks);
  assert.equal(history.past.length, 50);
  assert.equal(remember(undo.history, project.tracks).future.length, 0);
  fs.writeFileSync(process.argv[3], JSON.stringify(project, null, 2));
  console.log('TypeScript linked editing, validation, history, mute/solo and Rust contract passed.');
} finally {
  for (const name of ['project.mjs', 'timeline.mjs', 'analysis.mjs', 'edl.mjs', 'finishing.mjs']) { const file = path.join(temp, name); if (fs.existsSync(file)) fs.unlinkSync(file); }
  fs.rmdirSync(temp);
}
