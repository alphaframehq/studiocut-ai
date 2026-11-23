import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { stripTypeScriptTypes } from 'node:module';

const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'studiocut-analysis-'));
try {
  for (const name of ['project', 'timeline', 'analysis', 'edl', 'finishing']) {
    const source = fs.readFileSync(new URL(`../../src/${name}.ts`, import.meta.url), 'utf8');
    fs.writeFileSync(path.join(temp, `${name}.mjs`), stripTypeScriptTypes(source).replace(/'\.\/(project|timeline|analysis|edl|finishing)'/g, "'./$1.mjs'"));
  }
  const { rippleDelete, addCaptions } = await import(pathToFileURL(path.join(temp, 'analysis.mjs')));
  const { emptyHistory, remember, travel, editTimeline } = await import(pathToFileURL(path.join(temp, 'timeline.mjs')));
  const { validateProject } = await import(pathToFileURL(path.join(temp, 'project.mjs')));
  const project = validateProject(JSON.parse(fs.readFileSync(process.argv[2], 'utf8')));
  const cuts = [{ start: 63, end: 87 }, { start: 110, end: 120 }];
  const next = rippleDelete(project, cuts);
  assert.deepEqual(project.tracks.map(t => t.clips.length), [1, 1]);
  assert.deepEqual(next.tracks.map(t => t.clips.length), [3, 3]);
  assert.equal(next.tracks[0].clips[1].linkId, next.tracks[1].clips[1].linkId);
  const history = remember(emptyHistory(), project.tracks);
  const { project: undone, history: updated } = travel(next, history, 'undo');
  assert.deepEqual(undone.tracks, project.tracks);
  assert.equal(updated.past.length, 0);
  const locked = structuredClone(project); locked.tracks[1].locked = true;
  assert.throws(() => rippleDelete(locked, cuts), /Unlock/);
  assert.throws(() => rippleDelete(project, [{ start: 50, end: 80 }, { start: 60, end: 90 }]), /non-overlapping/);
  for (const preset of ['hormozi', 'minimal', 'documentary', 'neon']) {
    const captioned = addCaptions(project, project.tracks[0].clips[0].id, { timing: 'word', items: [{ text: 'Hello', start: 0.1, end: 0.8 }, { text: 'world', start: 2.1, end: 2.8 }] }, preset);
    assert.equal(captioned.schemaVersion, 4);
    const caption = captioned.tracks[2].clips[0];
    assert.equal(caption.caption.preset, preset);
    assert.equal(caption.start, 33);
    const split = editTimeline(captioned, { type: 'split', clipId: caption.id, frame: 60 });
    assert.equal(split.tracks[2].clips.length, 2);
    assert.equal(split.tracks[2].clips[1].sourceIn, 30);
    const ripple = rippleDelete(captioned, cuts);
    assert.equal(ripple.tracks[2].clips[1].sourceIn, 57);
    assert.equal(ripple.tracks[2].clips[1].caption.words[1].start, 63);
    assert.deepEqual(validateProject(JSON.parse(JSON.stringify(captioned))), captioned);
    const invalid = structuredClone(captioned); invalid.tracks[2].clips[0].caption.words[0].end = 0;
    assert.throws(() => validateProject(invalid), /timestamps/);
  }
  fs.writeFileSync(process.argv[3], JSON.stringify(next));
} finally { fs.rmSync(temp, { recursive: true, force: true }); }
