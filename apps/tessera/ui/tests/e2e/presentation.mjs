// The descriptor shared with Rust drives commands, helpers, tasks, edits and asks.
import { check, device, finish, mock, run, signIn } from './cdp.mjs';
run(async () => {
  await mock('/__mock/reset');
  const p = await device();
  await signIn(p, 'Presentation');
  const result = await p.eval(`__mod('lib/tools.ts').then(({toolPresentation, toolSummary, detailsExitCode}) => {
    const rows = [
      ['bash',{},'bash'], ['Bash',{},'bash'], ['PowerShell',{},'bash'],
      ['TaskOutput',{},'job'], ['job',{},'job'],
      ['Agent',{},'agent'], ['Task',{},'agent'], ['agents',{action:'start'},'agent'],
      ['agents',{action:'status'},'agents'], ['TodoWrite',{},'task'], ['TaskCreate',{},'task'],
      ['Edit',{},'edit'], ['Write',{},'write'], ['AskUserQuestion',{},'whiteboard']
    ];
    return {rows:rows.map(([n,a,k]) => [n,toolPresentation(n,a).kind,k]),
      command:toolSummary('Bash',JSON.stringify({command:'printf hello',description:'greet'})),
      helper:toolPresentation('Agent',{prompt:'work'}).helper,
      native:toolPresentation('agents',{action:'start'}).helper,
      legacyNative:toolPresentation('Agent',{tasks:[{name:'Legacy'}]}).helper,
      unknown:toolPresentation('FuturePlugin',{}),
      exit:detailsExitCode({exit_code:0}), missing:detailsExitCode({})};
  })`);
  for (const [name, actual, want] of result.rows) check(actual === want, `${name} uses shared ${want}`, actual);
  check(result.command === 'printf hello', 'command summary is the command, not CLI description', result.command);
  check(result.helper === 'read_only', 'CLI helper explicitly read-only', result.helper);
  check(result.native === 'controls', 'native helper explicitly controllable', result.native);
  check(result.legacyNative === 'controls', 'legacy native task-array helpers retain controls', result.legacyNative);
  check(result.unknown.kind === 'FuturePlugin', 'unknown tool is recognizable without invented kind', result.unknown);
  check(result.exit === 0, 'reported zero exit code kept', result.exit);
  check(result.missing == null, 'missing exit code stays unknown', result.missing);
  await p.close();
  finish();
});
