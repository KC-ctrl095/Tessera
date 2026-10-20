// Measure and click the final surface, not its entrance animation/transition.
// Await only this target's ancestry, so unrelated spinners cannot block a test.
export async function settled(p, expression) {
  await p.eval(`(async () => {
    const target = (${expression});
    if (!target) throw Error('Missing surface to settle');
    const running = () => {
      const animations = [];
      for (let e = target; e; e = e.parentElement) animations.push(...e.getAnimations().filter(a => a.playState === 'running'));
      return animations;
    };
    for (let attempt = 0; attempt < 8; attempt++) {
      await Promise.all(running().map(a => a.finished.catch(error => { if (error.name !== 'AbortError') throw error; })));
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      if (!target.isConnected) throw Error('Measured surface was removed');
      if (!running().length) return;
    }
    throw Error('Measured surface did not settle');
  })()`);
}
