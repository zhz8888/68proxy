import { readFileSync, appendFileSync, existsSync } from 'node:fs';

// 依赖版本巡检结果汇总：输出 Markdown 到日志并写入 GITHUB_STEP_SUMMARY。
// 用法：node dependency-report.mjs <npm-outdated.json> [<标签>=<cargo-outdated.json> ...]
//
// 输入约定（由 .github/workflows/dependency-check.yml 的检查步骤生成）：
//   - 检查正常：文件内为工具原样输出的 JSON
//   - 检查失败：生成同名 .error 标记文件，内容为失败原因
// 命令没跑起来（无文件/内容为空/标记文件存在）时任务必须失败，
// 否则会把「没查到」误报成「全部最新」。
const [npmPath, ...cargoArgs] = process.argv.slice(2);
const failOnOutdated = process.env.FAIL_ON_OUTDATED === 'true';

const KIND = { Normal: '运行时', Development: '开发时', Build: '构建时' };
const TYPE = { dependencies: '运行时', devDependencies: '开发时', optionalDependencies: '可选', peerDependencies: 'peer' };

const sections = [];
const errors = [];
let outdated = 0;

// 读取单个检查结果，失败时抛出可直接展示的原因
function readResult(path) {
  const marker = `${path}.error`;
  if (existsSync(marker)) {
    throw new Error(readFileSync(marker, 'utf8').trim() || '检查命令执行失败');
  }
  if (!existsSync(path)) throw new Error('未找到检查结果文件');
  const raw = readFileSync(path, 'utf8').trim();
  if (raw === '') throw new Error('输出为空，检查可能未成功执行');
  return raw;
}

function npmSection() {
  const data = JSON.parse(readResult(npmPath));
  const rows = Object.entries(data)
    .map(([name, info]) => ({ name, ...info }))
    .sort((a, b) => a.name.localeCompare(b.name));
  outdated += rows.length;
  if (rows.length === 0) {
    return ['### npm 依赖（`package.json`）', '', '✅ 全部依赖均为最新版本。'];
  }
  return [
    '### npm 依赖（`package.json`）',
    '',
    `发现 **${rows.length}** 个依赖存在新版本：`,
    '',
    '| 依赖 | 当前 | 范围内 | 最新 | 类型 |',
    '| --- | --- | --- | --- | --- |',
    ...rows.map(
      (r) =>
        `| \`${r.name}\` | ${r.current ?? '-'} | ${r.wanted ?? '-'} | **${r.latest}** | ${
          TYPE[r.dependencyType] ?? r.dependencyType ?? '-'
        } |`,
    ),
  ];
}

function cargoSection(label, path) {
  const rows = [];
  for (const line of readResult(path).split('\n').filter((l) => l.trim() !== '')) {
    const pkg = JSON.parse(line);
    for (const dep of pkg.dependencies ?? []) rows.push(dep);
  }
  rows.sort((a, b) => a.name.localeCompare(b.name));
  outdated += rows.length;
  if (rows.length === 0) {
    return [`### Rust 依赖（\`${label}\`）`, '', '✅ 全部依赖均为最新版本。'];
  }
  return [
    `### Rust 依赖（\`${label}\`）`,
    '',
    `发现 **${rows.length}** 个直接依赖存在新版本：`,
    '',
    '| 依赖 | 当前 | 范围内 | 最新 | 类型 | 升级方式 |',
    '| --- | --- | --- | --- | --- | --- |',
    ...rows.map(
      (r) =>
        `| \`${r.name}\` | ${r.project} | ${r.compat} | **${r.latest}** | ${KIND[r.kind] ?? r.kind ?? '-'} | ${
          r.compat === '---' ? '需放宽 Cargo.toml 版本要求' : '可直接升级'
        } |`,
    ),
  ];
}

try {
  sections.push(npmSection());
} catch (e) {
  errors.push(`npm 依赖检查未完成（\`package.json\`）：${e.message}`);
}

for (const arg of cargoArgs) {
  const eq = arg.indexOf('=');
  const label = arg.slice(0, eq);
  const path = arg.slice(eq + 1);
  try {
    sections.push(cargoSection(label, path));
  } catch (e) {
    errors.push(`Rust 依赖检查未完成（\`${label}\`）：${e.message}`);
  }
}

const lines = ['## 依赖版本巡检', ''];
if (errors.length > 0) lines.push('> ⚠️ 部分检查未成功执行，结果不完整。', '');
lines.push(...sections.map((s) => s.join('\n')).join('\n\n').split('\n'));
lines.push('');
if (errors.length === 0 && outdated === 0) {
  lines.push('**结论：所有直接依赖均已是最新版本。**');
} else if (errors.length === 0) {
  lines.push(`**结论：共 ${outdated} 个直接依赖存在新版本。**`);
}
for (const e of errors) lines.push(`- ❌ ${e}`);

const body = lines.join('\n');
console.log(body);
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${body}\n`);

if (errors.length > 0) {
  console.error(`::error::依赖检查未完整执行：${errors.join('；')}`);
  process.exit(1);
}
if (failOnOutdated && outdated > 0) {
  console.error(`::error::存在 ${outdated} 个非最新依赖（fail-on-outdated=true）`);
  process.exit(1);
}
