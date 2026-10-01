// uPlot charts that follow the page theme and their container's width.

const PALETTE = ['#2563eb', '#dc2626', '#16a34a', '#d97706', '#7c3aed', '#0891b2', '#db2777', '#65a30d', '#ea580c', '#475569'];
export const color = i => PALETTE[i % PALETTE.length];

const css = name => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

/**
 * chart(figure, {series: [{label, unit}], wall: bool, height, onZoom(t0, t1), sync: 'key', step: bool})
 * Returns the uPlot instance; update with u.setData([xs, ...ys]).
 */
export function chart(fig, opt) {
  const grid = { stroke: css('--line'), width: 1 };
  const axis = { stroke: css('--mute'), grid, ticks: grid };
  const units = [...new Set(opt.series.map(s => s.unit ?? ''))];
  const scales = { x: { time: !!opt.wall } };
  units.forEach((u, i) => (scales['y' + i] = { auto: true }));
  const u = new uPlot({
    width: fig.clientWidth || 600,
    height: opt.height ?? 220,
    scales,
    cursor: { sync: opt.sync ? { key: opt.sync } : undefined, drag: { x: true, y: false } },
    legend: { live: true },
    axes: [
      { ...axis, ...(opt.wall ? {} : { values: (_, v) => v.map(x => x + ' s') }) },
      ...units.map((unit, i) => ({ ...axis, scale: 'y' + i, side: i % 2 ? 1 : 3, label: unit || undefined, labelSize: unit ? 18 : 0, size: 54 })),
    ],
    series: [
      { label: opt.wall ? 'time' : 't', value: (_, v) => v == null ? '–' : opt.wall ? new Date(v * 1000).toLocaleTimeString([], { hour12: false, fractionalSecondDigits: 3 }) : v.toFixed(4) + ' s' },
      ...opt.series.map((s, i) => ({
        label: s.label, stroke: s.color ?? color(i), width: 1.5, scale: 'y' + units.indexOf(s.unit ?? ''),
        paths: opt.step ? uPlot.paths.stepped({ align: 1 }) : undefined, spanGaps: true,
        value: (_, v) => v == null ? '–' : (Number.isInteger(v) ? v : +v.toPrecision(6)) + (s.unit ? ' ' + s.unit : ''),
      })),
    ],
    hooks: {
      setSelect: [uu => {
        if (uu.select.width > 4 && opt.onZoom) {
          const a = uu.posToVal(uu.select.left, 'x'), b = uu.posToVal(uu.select.left + uu.select.width, 'x');
          uu.setSelect({ width: 0, height: 0 }, false);
          opt.onZoom(a, b);
        }
      }],
    },
  }, [[], ...opt.series.map(() => [])], fig);
  if (opt.onZoom) {
    // zooming is ours (re-query at the new range); keep uPlot from zooming itself
    u.cursor.drag.setScale = false;
    u.over.addEventListener('dblclick', () => opt.onZoom(null, null));
  }
  const ro = new ResizeObserver(() => u.setSize({ width: fig.clientWidth, height: opt.height ?? 220 }));
  ro.observe(fig);
  u._ro = ro;
  return u;
}

export function destroy(u) {
  u?._ro?.disconnect();
  u?.destroy();
}

/**
 * Merge per-signal [t, v, t, v...] arrays (different timestamps) into
 * uPlot's aligned [xs, ys1, ys2...] with nulls where a signal has no sample.
 */
export function align(seriesList) {
  if (seriesList.length === 1) {
    const a = seriesList[0], n = a.length / 2, xs = new Float64Array(n), ys = new Array(n);
    for (let i = 0; i < n; i++) { xs[i] = a[2 * i]; ys[i] = a[2 * i + 1]; }
    return [xs, ys];
  }
  const idx = seriesList.map(() => 0);
  const xs = [], ys = seriesList.map(() => []);
  for (;;) {
    let t = Infinity;
    for (let k = 0; k < seriesList.length; k++) if (idx[k] < seriesList[k].length) t = Math.min(t, seriesList[k][idx[k]]);
    if (t === Infinity) break;
    xs.push(t);
    for (let k = 0; k < seriesList.length; k++) {
      const a = seriesList[k];
      if (idx[k] < a.length && a[idx[k]] === t) { ys[k].push(a[idx[k] + 1]); idx[k] += 2; }
      else ys[k].push(null);
    }
  }
  return [xs, ...ys];
}
