import { execFileSync } from 'node:child_process';
import { homedir, hostname, userInfo } from 'node:os';

const files = execFileSync('git', ['diff', '--cached', '--name-only', '--diff-filter=ACMR', '-z'])
  .toString()
  .split('\0')
  .filter(Boolean);
const personal_values = [homedir(), hostname(), userInfo().username].filter(
  (value) => value.length > 2,
);
const patterns = [
  /\/(?:Users|home)\/[^\s/]+/,
  /[A-Za-z]:\\Users\\[^\\\s]+/,
  /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/,
  /\bsk-[A-Za-z0-9_-]{20,}\b/,
];
const rejected: string[] = [];
for (const file of files) {
  const data = execFileSync('git', ['show', `:${file}`], { maxBuffer: 16 * 1024 * 1024 });
  if (data.includes(0)) continue;
  const text = data.toString('utf8');
  if (
    personal_values.some((value) => text.includes(value)) ||
    patterns.some((pattern) => pattern.test(text))
  ) {
    rejected.push(file);
  }
}
if (rejected.length) {
  console.error(`Review possible personal information or credentials in:\n${rejected.join('\n')}`);
  process.exitCode = 1;
} else {
  console.log(
    `Checked ${files.length} staged files for local identity, machine paths, and common secret patterns.`,
  );
}
