// Issue 状态标签与「等对方回应」自动关闭。由 .github/workflows/issue-status.yml 经
// actions/github-script 调用；判定部分是纯函数，用例见同目录 issue-status.test.js。

const DAY = 24 * 60 * 60 * 1000;
const WARN_DAYS = 14; // 维护者回复后对方多久未回应即提醒
const CLOSE_DAYS = 7; // 提醒后再等多久关闭

const STALE = '即将关闭';

// 状态标签彼此互斥（STALE 是自动化标记，不在其中）；不挂状态即「尚未处理」。
// exempt：维护者已接手，不参与自动关闭。waiting：在等提交者，提交者一回复即撤掉。
const STATUS_LABELS = [
  { name: '状态: 待反馈', color: 'fbca04', description: '需要提交者补充信息或验证修复', waiting: true },
  { name: '状态: 无法重现', color: 'fef2c0', description: '按现有描述无法复现，需要提交者补充环境信息', waiting: true },
  { name: '状态: 已计划', color: '1d76db', description: '会做：含开发中、已修复待发布与远期计划', exempt: true },
  { name: '状态: 暂无计划', color: 'bfdadc', description: '方向认可，但近期不安排，保留开启留档', exempt: true },
];
// 类型标签：from 是 GitHub 默认的英文名，首次同步时原地改名，已挂的 issue 不受影响
const TYPE_LABELS = [
  { name: '缺陷', from: 'bug', color: 'd73a4a', description: '功能异常' },
  { name: '功能建议', from: 'enhancement', color: 'a2eeef', description: '新功能或改进建议' },
  { name: '使用问题', from: 'question', color: 'd876e3', description: '使用方法咨询' },
  { name: '文档', from: 'documentation', color: '0075ca', description: '文档补充或修正' },
  { name: '欢迎贡献', from: 'help wanted', color: '008672', description: '欢迎提交 PR' },
  { name: '适合新手', from: 'good first issue', color: '7057ff', description: '适合初次贡献' },
  { name: '重复', from: 'duplicate', color: 'cfd3d7', description: '已有相同的 issue' },
  { name: '无效', from: 'invalid', color: 'e4e669', description: '不是有效的问题' },
  { name: '不予处理', from: 'wontfix', color: 'ffffff', description: '不会处理' },
];
const ALL_LABELS = [
  ...TYPE_LABELS,
  ...STATUS_LABELS,
  { name: STALE, color: 'b60205', description: '维护者回复后长期无回应，到期自动关闭' },
];

// 机器人评论里的标记，用来认出「提醒」和「关闭」那一条
const WARN_MARKER = '<!-- issue-status:stale-warning -->';
const CLOSE_MARKER = '<!-- issue-status:stale-close -->';
const MAINTAINER_ASSOC = new Set(['OWNER', 'MEMBER', 'COLLABORATOR']);

const isStatus = (name) => STATUS_LABELS.some((l) => l.name === name);
const isExempt = (name) => STATUS_LABELS.some((l) => l.name === name && l.exempt);
const isWaiting = (name) => STATUS_LABELS.some((l) => l.name === name && l.waiting);
const isBot = (c) => c.user?.type === 'Bot';
const isMaintainer = (c) => MAINTAINER_ASSOC.has(c.author_association);

/**
 * 定时巡检时对单个 open issue 的处理决定。
 * @param issue    REST issue 对象（labels、user、author_association）
 * @param comments 按时间升序的 REST 评论
 * @returns 'warn' | 'close' | 'unstale' | 'none'
 */
function decide(issue, comments, now) {
  const labels = issue.labels.map((l) => (typeof l === 'string' ? l : l.name));
  const stale = labels.includes(STALE);
  // 维护者自己开的帖（公告、计划）不在此列
  if (isMaintainer(issue)) return stale ? 'unstale' : 'none';
  if (labels.some(isExempt)) return stale ? 'unstale' : 'none';

  const human = comments.filter((c) => !isBot(c));
  const last = human[human.length - 1];
  const waitingOnReporter = last && isMaintainer(last) && last.user.login !== issue.user.login;
  if (!waitingOnReporter) return stale ? 'unstale' : 'none';

  const lastAt = Date.parse(last.created_at);
  // 只认最后一条维护者回复之后的标记评论；维护者又补了话就重新计时
  const mark = comments.filter(
    (c) => isBot(c) && Date.parse(c.created_at) > lastAt
      && (c.body?.includes(WARN_MARKER) || c.body?.includes(CLOSE_MARKER)),
  ).pop();

  // 自动关闭后被维护者手动重开：不再处理
  if (mark?.body.includes(CLOSE_MARKER)) return stale ? 'unstale' : 'none';
  if (mark) {
    // 维护者手动撤掉「即将关闭」视为本轮不再追究
    if (!stale) return 'none';
    return now - Date.parse(mark.created_at) >= CLOSE_DAYS * DAY ? 'close' : 'none';
  }
  if (now - lastAt >= WARN_DAYS * DAY) return 'warn';
  return stale ? 'unstale' : 'none';
}

const warnBody = (author) => `${WARN_MARKER}
@${author} 维护者回复后已有 ${WARN_DAYS} 天没有收到回应。如果问题仍然存在，请补充信息或回复一下；${CLOSE_DAYS} 天内仍无回应将自动关闭。`;

const closeBody = `${CLOSE_MARKER}
由于长时间没有收到回应，此问题已自动关闭。如果问题仍然存在，提交者在此回复即会自动重新打开。`;

async function ensureLabels({ github, context, core }) {
  const repo = context.repo;
  const existing = await github.paginate(github.rest.issues.listLabelsForRepo, { ...repo, per_page: 100 });
  const byName = new Map(existing.map((l) => [l.name, l]));
  for (const l of ALL_LABELS) {
    const cur = byName.get(l.name);
    const want = { ...repo, name: l.name, color: l.color, description: l.description };
    if (!cur && l.from && byName.has(l.from)) {
      await github.rest.issues.updateLabel({ ...want, name: l.from, new_name: l.name });
      core.info(`改名 ${l.from} → ${l.name}`);
    } else if (!cur) {
      await github.rest.issues.createLabel(want);
      core.info(`创建标签 ${l.name}`);
    } else if (cur.color !== l.color || (cur.description || '') !== l.description) {
      await github.rest.issues.updateLabel(want);
      core.info(`更新标签 ${l.name}`);
    }
  }
}

async function removeLabel(github, repo, number, name) {
  try {
    await github.rest.issues.removeLabel({ ...repo, issue_number: number, name });
  } catch (e) {
    if (e.status !== 404) throw e;
  }
}

async function sweep({ github, context, core }, dryRun) {
  const repo = context.repo;
  const now = Date.now();
  const issues = await github.paginate(github.rest.issues.listForRepo, { ...repo, state: 'open', per_page: 100 });
  for (const issue of issues) {
    if (issue.pull_request) continue;
    const comments = await github.paginate(github.rest.issues.listComments, {
      ...repo, issue_number: issue.number, per_page: 100,
    });
    let action = decide(issue, comments, now);
    if (action === 'close' && !dryRun) {
      // 关闭前重取一次评论，避开提交者恰在此刻回复
      const fresh = await github.paginate(github.rest.issues.listComments, {
        ...repo, issue_number: issue.number, per_page: 100,
      });
      action = decide(issue, fresh, Date.now());
    }
    if (action === 'none') continue;
    core.info(`#${issue.number} ${action}${dryRun ? '（dry run）' : ''}  ${issue.title}`);
    if (dryRun) continue;
    const n = { ...repo, issue_number: issue.number };
    if (action === 'warn') {
      await github.rest.issues.addLabels({ ...n, labels: [STALE] });
      await github.rest.issues.createComment({ ...n, body: warnBody(issue.user.login) });
    } else if (action === 'close') {
      await github.rest.issues.createComment({ ...n, body: closeBody });
      await github.rest.issues.update({ ...n, state: 'closed', state_reason: 'not_planned' });
    } else if (action === 'unstale') {
      await removeLabel(github, repo, issue.number, STALE);
    }
  }
}

// 非维护者评论：撤销「即将关闭」；提交者回复时撤掉在等他的状态，回到「尚未处理」。
// 被自动关闭的 issue 由提交者回复时顺带重开。
async function onComment({ github, context }) {
  const { issue, comment } = context.payload;
  if (issue.pull_request || isBot(comment) || isMaintainer(comment)) return;
  const repo = context.repo;
  const labels = issue.labels.map((l) => l.name);
  if (issue.state === 'closed') {
    const autoClosed = labels.includes(STALE) && issue.state_reason === 'not_planned';
    if (!autoClosed || comment.user.login !== issue.user.login) return;
    await github.rest.issues.update({ ...repo, issue_number: issue.number, state: 'open' });
  }
  if (labels.includes(STALE)) await removeLabel(github, repo, issue.number, STALE);
  const waiting = labels.filter(isWaiting);
  if (waiting.length && comment.user.login === issue.user.login) {
    for (const name of waiting) await removeLabel(github, repo, issue.number, name);
  }
}

// 手动加某个状态时撤掉其它状态，加了豁免状态再撤「即将关闭」
async function onIssue({ github, context }) {
  const { issue, label } = context.payload;
  const repo = context.repo;
  if (isStatus(label.name)) {
    // payload 里的标签是事件发生时的快照，连加两个状态时各自的 run 会互删，故重取现状；
    // 新加的状态已被后来者撤掉，就由后来者那次 run 负责
    const current = (await github.paginate(github.rest.issues.listLabelsOnIssue, {
      ...repo, issue_number: issue.number, per_page: 100,
    })).map((l) => l.name);
    if (!current.includes(label.name)) return;
    for (const name of current) {
      if (name !== label.name && isStatus(name)) await removeLabel(github, repo, issue.number, name);
    }
    if (isExempt(label.name) && current.includes(STALE)) await removeLabel(github, repo, issue.number, STALE);
  }
}

module.exports = {
  decide, ensureLabels, sweep, onComment, onIssue,
  STALE, WARN_MARKER, CLOSE_MARKER, WARN_DAYS, CLOSE_DAYS, DAY,
};
