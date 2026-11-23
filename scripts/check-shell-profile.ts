import { loadShellAccountEnvironment } from './shell-account.ts';

try {
  await loadShellAccountEnvironment(process.env, undefined, true);
  console.log('Shell build profile enables encrypted API-key sync.');
} catch (error) {
  console.error(
    error instanceof Error ? error.message : 'Could not validate the shell build profile.',
  );
  process.exitCode = 1;
}
