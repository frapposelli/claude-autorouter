(() => {
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  if (reduce) document.documentElement.classList.add('reduce');

  // Copy buttons
  const copy = async (text, btn) => {
    try {
      await navigator.clipboard.writeText(text);
      const old = btn.textContent;
      btn.textContent = 'Copied';
      setTimeout(() => (btn.textContent = old), 1400);
    } catch {
      btn.textContent = 'Press ⌘C';
    }
  };
  $$('[data-copy]').forEach((b) => b.addEventListener('click', () => copy(b.dataset.copy, b)));

  // Routing demo. Illustrative rules only; the real verdict comes from the evaluator model.
  const task = $('#task');
  const auto = $('#auto');
  const status = $('#status');
  const reason = $('#reason');
  const models = $$('#models .model');
  const hard = /(architect|design|migrat|race condition|deadlock|security|concurren|rewrite|redesign|event-driven|root cause)/i;
  const easy = /(typo|rename|comment|format|lint|whitespace|spelling|bump|readme)/i;
  const verdict = (text) => {
    const t = text.trim();
    if (!t) return null;
    if (hard.test(t) || t.length > 140) return 'opus';
    if (easy.test(t) && t.length < 90) return 'haiku';
    return 'sonnet';
  };
  const label = { haiku: 'Haiku', sonnet: 'Sonnet 5.5', opus: 'Opus 5.5' };
  const render = () => {
    let v = verdict(task.value);
    models.forEach((m) => m.classList.remove('on'));
    if (!v) {
      status.innerHTML = '<span class="dot">●</span> AutoRouter · waiting for a task';
      reason.textContent = 'Type a task, or pick an example.';
      return;
    }
    let floor = false;
    let used = v;
    if (auto.checked && v === 'haiku') { used = 'sonnet'; floor = true; }
    $(`#models [data-m="${used}"]`).classList.add('on');
    status.innerHTML = `<span class="dot">●</span> AutoRouter · ${label[used]} selected${floor ? ' · Auto floor from Haiku' : ''}`;
    reason.textContent = {
      haiku: 'A small, contained edit. Haiku is enough.',
      sonnet: 'Ordinary feature work. Sonnet handles it.',
      opus: 'Open-ended design or debugging. Worth the strongest model.',
    }[v] + (floor ? ' Auto permission mode switches between Sonnet and Opus, so a Haiku verdict runs on Sonnet.' : '');
  };
  task.addEventListener('input', () => { $$('.chip').forEach((c) => c.setAttribute('aria-pressed', 'false')); render(); });
  auto.addEventListener('change', render);
  $$('.chip').forEach((c) => c.addEventListener('click', () => {
    task.value = c.dataset.task;
    $$('.chip').forEach((x) => x.setAttribute('aria-pressed', String(x === c)));
    render();
  }));
  render();

  // Pinned story: the three statements change the pinned fragment.
  const steps = $$('.story-step');
  const rows = $$('#pin .row');
  const states = [
    { on: [0], dim: [1, 2, 3] },
    { on: [1, 2], dim: [3] },
    { on: [3], dim: [] },
  ];
  const setStep = (i) => {
    steps.forEach((s, k) => s.classList.toggle('on', k === i));
    rows.forEach((r, k) => {
      const st = states[i];
      r.classList.toggle('hl', st.on.includes(k));
      r.classList.toggle('dim', st.dim.includes(k));
    });
  };
  setStep(0);
  if ('IntersectionObserver' in window) {
    const io = new IntersectionObserver((es) => es.forEach((e) => {
      if (e.isIntersecting) setStep(Number(e.target.dataset.step));
    }), { rootMargin: '-45% 0px -45% 0px' });
    steps.forEach((s) => io.observe(s));
  } else steps.forEach((s) => s.classList.add('on'));

  // FAQ: one answer at a time; mobile shows it under the question.
  const qs = $$('.q');
  const answers = $$('.ans');
  $$('.ans-m').forEach((p, i) => { p.innerHTML = answers[i].innerHTML; });
  const select = (i) => {
    qs.forEach((q, k) => {
      q.setAttribute('aria-expanded', String(k === i));
      const m = q.nextElementSibling;
      m.hidden = k !== i;
    });
    answers.forEach((a, k) => a.classList.toggle('on', k === i));
  };
  qs.forEach((q, i) => {
    q.addEventListener('click', () => select(i));
    q.addEventListener('mouseenter', () => { if (matchMedia('(min-width: 901px)').matches) select(i); });
    q.addEventListener('focus', () => { if (matchMedia('(min-width: 901px)').matches) select(i); });
  });
  select(0);

  // CTA dialog
  const code = $('#cta-code');
  const cmds = {
    ollama: 'npm install -g claude-autorouter\nclaude-autorouter setup --pull\nclaude-autorouter claude',
    jev: 'npm install -g claude-autorouter\nclaude-autorouter setup --evaluator jev\nclaude-autorouter claude',
  };
  let current = 'ollama';
  $$('[data-ev]').forEach((b) => b.addEventListener('click', () => {
    current = b.dataset.ev;
    $$('[data-ev]').forEach((x) => x.setAttribute('aria-pressed', String(x === b)));
    code.textContent = cmds[current];
  }));
  const cb = $('#cta-copy');
  cb.addEventListener('click', () => copy(cmds[current], cb));

  // Entrances
  const rise = $$('.rise');
  if (!reduce && 'IntersectionObserver' in window) {
    const io = new IntersectionObserver((es) => es.forEach((e) => {
      if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); }
    }), { threshold: 0.12 });
    rise.forEach((el, i) => { el.style.setProperty('--i', String(i % 3)); io.observe(el); });
  } else rise.forEach((el) => el.classList.add('in'));
})();
