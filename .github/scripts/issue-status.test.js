// node --test .github/scripts/issue-status.test.js
const test = require('node:test');
const assert = require('node:assert/strict');
const { decide, STALE, WARN_MARKER, CLOSE_MARKER, DAY } = require('./issue-status.js');

const NOW = Date.parse('2026-10-21T00:00:00Z');
const ago = (days) => new Date(NOW - days * DAY).toISOString();

const reporter = { login: 'alice', type: 'User' };
const owner = { login: 'huanfeng', type: 'User' };
const bot = { login: 'github-actions[bot]', type: 'Bot' };

const issue = (labels = [], extra = {}) => ({
  user: reporter, author_association: 'NONE', labels: labels.map((name) => ({ name })), ...extra,
});
const byOwner = (days) => ({ user: owner, author_association: 'OWNER', created_at: ago(days), body: '请补充日志' });
const byReporter = (days) => ({ user: reporter, author_association: 'NONE', created_at: ago(days), body: '日志在此' });
const closing = (days) => ({ user: bot, author_association: 'NONE', created_at: ago(days), body: `${CLOSE_MARKER}\n已关闭` });
const warning = (days) => ({ user: bot, author_association: 'NONE', created_at: ago(days), body: `${WARN_MARKER}\n提醒` });

test('维护者回复满 14 天无回应则提醒', () => {
  assert.equal(decide(issue(), [byReporter(20), byOwner(14)], NOW), 'warn');
});

test('不满 14 天不动', () => {
  assert.equal(decide(issue(), [byOwner(13)], NOW), 'none');
});

test('最后是提交者发言不进入流程', () => {
  assert.equal(decide(issue(), [byOwner(30), byReporter(20)], NOW), 'none');
});

test('没有任何评论不进入流程', () => {
  assert.equal(decide(issue(), [], NOW), 'none');
});

test('提醒满 7 天关闭', () => {
  assert.equal(decide(issue([STALE]), [byOwner(21), warning(7)], NOW), 'close');
});

test('提醒不满 7 天继续等', () => {
  assert.equal(decide(issue([STALE]), [byOwner(20), warning(6)], NOW), 'none');
});

test('机器人评论不算对方回应', () => {
  assert.equal(decide(issue([STALE]), [byOwner(21), warning(7), { ...warning(1), body: '其它机器人' }], NOW), 'close');
});

test('提醒后提交者回复则撤销标记', () => {
  assert.equal(decide(issue([STALE]), [byOwner(21), warning(7), byReporter(1)], NOW), 'unstale');
});

test('提醒后维护者又补话，从新回复重新计时', () => {
  assert.equal(decide(issue([STALE]), [byOwner(30), warning(16), byOwner(3)], NOW), 'unstale');
  assert.equal(decide(issue([STALE]), [byOwner(40), warning(26), byOwner(15)], NOW), 'warn');
});

test('豁免状态不参与', () => {
  for (const l of ['状态: 已计划', '状态: 暂无计划']) {
    assert.equal(decide(issue([l]), [byOwner(60)], NOW), 'none', l);
  }
});

test('豁免状态挂着即将关闭则撤销', () => {
  assert.equal(decide(issue(['状态: 已计划', STALE]), [byOwner(30), warning(8)], NOW), 'unstale');
});

test('等对方状态照常参与', () => {
  assert.equal(decide(issue(['状态: 无法重现']), [byOwner(14)], NOW), 'warn');
});

test('维护者自己开的帖不参与', () => {
  assert.equal(decide(issue([], { user: owner, author_association: 'OWNER' }), [byOwner(60)], NOW), 'none');
});

test('提交者本身是协作者时不进入流程', () => {
  const collab = { login: 'bob', type: 'User' };
  const i = issue([], { user: collab, author_association: 'COLLABORATOR' });
  assert.equal(decide(i, [{ user: collab, author_association: 'COLLABORATOR', created_at: ago(30) }], NOW), 'none');
});

test('自动关闭后维护者手动重开，不再关闭', () => {
  assert.equal(decide(issue([STALE]), [byOwner(40), warning(26), closing(19)], NOW), 'unstale');
  assert.equal(decide(issue(), [byOwner(40), warning(26), closing(19)], NOW), 'none');
});

test('维护者手动撤掉即将关闭，本轮不再提醒', () => {
  assert.equal(decide(issue(), [byOwner(40), warning(26)], NOW), 'none');
});
