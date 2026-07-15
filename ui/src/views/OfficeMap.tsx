import React, { useEffect, useMemo, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import type { Project } from './Board';
import {
  DESK,
  PersonaPresence,
  isAuditLive,
  isDraftingFamilyActivity,
  isResearchLive,
  clampMaxWorkers,
  occupiedCount,
  presenceFor,
  stationsFor,
  tierFor,
} from '../lib/officeLayout';

/**
 * The pixel virtual office — the default project view. A projection of the board snapshot
 * onto a pool of 10 worker personas (office-core persona.rs), rendered with the generated
 * sprite set (ui/public/sprites, blueprint: poc/office). Layout + presence come from the pure
 * helpers in ../lib/officeLayout; this file owns only the rendering + animation timing.
 *
 * Theme: the floor and character sprites are ART with their own baked palette — they are NOT
 * theme tokens. Everything else (name tags, task lines, legend, wall board) uses --wf-* tokens
 * so the chrome follows koma's live theme.
 */

const S = './sprites/';

// 3x-scaled sprite sizes (native 16px art -> 48px), matching the POC.
const WORKER = 48;
const CHAIR = 48;
const DESK_W = 96;
const DESK_H = 48;
const MON_W = 48;
const MON_H = 36;
const BUB_W = 36;
const BUB_H = 30;
const STAFF = 48;

interface OfficeMapProps {
  project: Project;
  onTaskClick: (taskId: string) => void;
}

/** Single matchMedia check for reduced motion (jsdom-safe). When true, every interval is
 * skipped and the office renders its static first frame. */
function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return;
    const mq = window.matchMedia('(prefers-reduced-motion: reduce)');
    setReduced(mq.matches);
    const onChange = (e: MediaQueryListEvent) => setReduced(e.matches);
    // addEventListener is the modern API; fall back to the deprecated addListener for old webviews.
    if (mq.addEventListener) mq.addEventListener('change', onChange);
    else mq.addListener(onChange);
    return () => {
      if (mq.removeEventListener) mq.removeEventListener('change', onChange);
      else mq.removeListener(onChange);
    };
  }, []);
  return reduced;
}

/** A single monotonic animation clock. All frames derive from this one tick, so the office
 * needs exactly one interval; `enabled=false` (reduced motion) freezes it at 0. */
function useTick(baseMs: number, enabled: boolean): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    const id = setInterval(() => setTick((t) => (t + 1) % 1_000_000), baseMs);
    return () => clearInterval(id);
  }, [baseMs, enabled]);
  return tick;
}

function dotColor(status: PersonaPresence['status'], waiting: boolean): string {
  if (waiting) return 'var(--wf-warn)';
  switch (status) {
    case 'working': return 'var(--wf-info)';
    case 'review-debate': return 'var(--wf-review)';
    case 'parked': return 'var(--wf-warn)';
    default: return 'var(--wf-dim)';
  }
}

const Sprite: React.FC<{ src: string; left: number; top: number; w: number; h: number; z: number; flip?: boolean }> = ({
  src, left, top, w, h, z, flip,
}) => (
  <img
    src={src}
    alt=""
    aria-hidden
    style={{
      position: 'absolute',
      left,
      top,
      width: w,
      height: h,
      zIndex: z,
      imageRendering: 'pixelated',
      pointerEvents: 'none',
      transform: flip ? 'scaleX(-1)' : undefined,
    }}
  />
);

export const OfficeMap: React.FC<OfficeMapProps> = ({ project, onTaskClick }) => {
  const reduced = usePrefersReducedMotion();
  const tick = useTick(200, !reduced);

  const presence = useMemo(() => presenceFor(project as any), [project]);
  const occupied = useMemo(() => presence.filter((p) => p.status !== 'idle'), [presence]);
  const maxWorkers = clampMaxWorkers(project?.config?.maxWorkers);
  // Grow the room to fit however many personas are occupied — an occupied desk is never
  // dropped when maxWorkers (the tier's base) is smaller.
  const tier = tierFor(Math.max(maxWorkers, occupiedCount(presence)));
  const desks = useMemo(() => stationsFor(tier), [tier]);

  // Derived animation frames (all from the single tick).
  const typeFrame = Math.floor(tick / 2) % 2 === 0 ? 'type_a' : 'type_b';
  const blinkOn = tick % 7 !== 6; // occasional dark flicker reads as screen activity
  const blinkFrame = Math.floor(tick / 2) % 2 === 0 ? 'a' : 'b';
  const beat = Math.floor(tick / 4); // ~800ms debate beat
  const readFrame = Math.floor(tick / 3) % 2 === 0 ? 'reading_a' : 'reading_b';
  const judgeFrame = Math.floor(tick / 3) % 2 === 0 ? 'a' : 'b';

  // Container bounds: desks grid + a bottom lane for the PM + side margins for the fixed staff.
  const cols = desks.reduce((m, d) => Math.max(m, d.x), 0);
  const rows = desks.reduce((m, d) => Math.max(m, d.y), 0);
  const width = Math.max(cols + DESK.cellW + DESK.padX, 520) + 120; // +120 left bookshelf lane
  const height = rows + DESK.cellH + DESK.padY + 120; // +120 bottom PM lane
  const floorLeft = 120; // desks + staff shift right of the bookshelf lane

  const phaseKind = project?.phase?.kind;
  const pmPacing = phaseKind === 'drafting' || phaseKind === 'ready';
  const researchLive = isResearchLive(project as any);
  const auditLive = isAuditLive(project as any);

  // PM: paces the bottom lane while drafting/ready, else stands by the wall board (running+).
  const pmLaneY = height - 84;
  const pmSweep = Math.max(120, width - STAFF - floorLeft - 80);
  const pmPeriod = 48;
  const pmPhase = tick % (2 * pmPeriod);
  const pmT = pmPhase < pmPeriod ? pmPhase / pmPeriod : (2 * pmPeriod - pmPhase) / pmPeriod;
  const pmX = pmPacing ? floorLeft + 20 + pmT * pmSweep : width - STAFF - 60;
  const pmStep = tick % 2 === 0 ? 'pm_walk_a' : 'pm_walk_b';
  const pmFlip = pmPacing && pmPhase >= pmPeriod;

  return (
    <div style={{ padding: '0.25rem 0 1rem' }}>
      <div style={{ overflowX: 'auto', paddingBottom: '0.5rem' }}>
        <motion.div
          layout
          transition={{ duration: 0.35, ease: 'easeInOut' }}
          style={{
            position: 'relative',
            width,
            height,
            backgroundColor: 'var(--wf-panel2)',
            backgroundImage: `url("${S}floor_tile.png")`,
            backgroundSize: `${16 * 3}px ${16 * 3}px`,
            imageRendering: 'pixelated',
            border: '1px solid var(--wf-border)',
            borderRadius: 'var(--wf-radius)',
            overflow: 'hidden',
          }}
        >
          {/* Bookshelf lane (left) — a quiet chrome strip that houses the researcher desk. */}
          <div
            style={{
              position: 'absolute',
              left: 0,
              top: 0,
              bottom: 0,
              width: floorLeft,
              borderRight: '1px solid var(--wf-border)',
              background: 'var(--wf-panel)',
              opacity: 0.7,
              zIndex: 0,
            }}
          />

          {/* Desks: occupied personas packed first, remaining desks empty ("capacity free"). */}
          <AnimatePresence initial={false}>
            {desks.map((desk, i) => {
              const p = occupied[i];
              const stationLeft = floorLeft + desk.x;
              const isReview = p?.status === 'review-debate';
              const workerTurn = beat % 2 === 0;
              const clickable = p ? p.taskId : undefined;

              const monitorSrc = p && p.monitorActive
                ? `${S}monitor_on_${blinkOn ? blinkFrame : 'a'}.png`
                : `${S}monitor_off.png`;
              // Worker frame: typing while working+live, otherwise seated idle. In a debate the
              // worker alternates defending (type) / listening (idle) with the beat.
              let workerFrame = 'idle';
              if (p && p.status === 'working' && p.monitorActive && !reduced) workerFrame = typeFrame;
              else if (isReview) workerFrame = workerTurn ? 'type_a' : 'idle';

              return (
                <motion.div
                  key={desk.index}
                  layout
                  initial={{ opacity: 0 }}
                  animate={{ opacity: 1 }}
                  exit={{ opacity: 0 }}
                  transition={{ duration: 0.3 }}
                  onClick={clickable ? () => onTaskClick(clickable) : undefined}
                  style={{
                    position: 'absolute',
                    left: stationLeft,
                    top: desk.y,
                    width: DESK.cellW,
                    height: DESK.cellH,
                    cursor: clickable ? 'pointer' : 'default',
                    zIndex: 2,
                  }}
                  data-testid="office-desk"
                  data-persona={p?.persona}
                  data-status={p?.status ?? 'absent'}
                >
                  <Sprite src={`${S}chair.png`} left={60} top={96} w={CHAIR} h={CHAIR} z={1} />
                  {p && (
                    <Sprite src={`${S}worker_${p.persona}_${workerFrame}.png`} left={60} top={78} w={WORKER} h={WORKER} z={2} />
                  )}
                  {isReview && (
                    <Sprite
                      src={`${S}reviewer_point_${workerTurn ? 'a' : 'b'}.png`}
                      left={108}
                      top={74}
                      w={WORKER}
                      h={WORKER}
                      z={4}
                    />
                  )}
                  <Sprite src={`${S}desk.png`} left={36} top={44} w={DESK_W} h={DESK_H} z={3} />
                  <Sprite src={monitorSrc} left={60} top={22} w={MON_W} h={MON_H} z={4} />

                  {isReview && !reduced && (
                    <React.Fragment>
                      {/* Alternating debate: worker's "!" on their beat, reviewer's "?" on the other. */}
                      <img
                        src={`${S}bubble_excl.png`}
                        alt=""
                        aria-hidden
                        style={{
                          position: 'absolute', left: 44, top: 52, width: BUB_W, height: BUB_H, zIndex: 6,
                          imageRendering: 'pixelated', pointerEvents: 'none',
                          visibility: workerTurn ? 'visible' : 'hidden',
                        }}
                      />
                      <img
                        src={`${S}bubble_q.png`}
                        alt=""
                        aria-hidden
                        style={{
                          position: 'absolute', left: 120, top: 48, width: BUB_W, height: BUB_H, zIndex: 6,
                          imageRendering: 'pixelated', pointerEvents: 'none',
                          visibility: workerTurn ? 'hidden' : 'visible',
                        }}
                      />
                    </React.Fragment>
                  )}

                  {/* Name tag + task line — single-line ellipsized, chrome tokens. */}
                  <div
                    style={{
                      position: 'absolute', top: 150, left: 0, width: DESK.cellW, textAlign: 'center',
                      fontSize: '0.7rem', color: 'var(--wf-fg)', whiteSpace: 'nowrap', overflow: 'hidden',
                      textOverflow: 'ellipsis', zIndex: 5,
                    }}
                  >
                    <span style={{
                      display: 'inline-block', width: 6, height: 6, borderRadius: '50%', marginRight: 5,
                      verticalAlign: 1, background: dotColor(p?.status ?? 'idle', Boolean(p?.waitingForChair)),
                    }} />
                    {p ? p.persona : '—'}
                    {isReview ? ' + reviewer' : ''}
                  </div>
                  <div
                    style={{
                      position: 'absolute', top: 168, left: 0, width: DESK.cellW, textAlign: 'center',
                      fontSize: '0.62rem', color: 'var(--wf-dim)', whiteSpace: 'nowrap', overflow: 'hidden',
                      textOverflow: 'ellipsis', zIndex: 5,
                    }}
                    title={p?.taskTitle}
                  >
                    {p
                      ? p.waitingForChair
                        ? 'waiting for a chair'
                        : p.status === 'parked'
                          ? `parked — ${p.taskTitle ?? ''}`
                          : p.taskTitle ?? ''
                      : 'capacity free'}
                  </div>
                </motion.div>
              );
            })}
          </AnimatePresence>

          {/* Researcher — bookshelf lane; reads while research is in flight, else stands idle. */}
          <div style={{ position: 'absolute', left: 18, top: 60, width: 84, zIndex: 2 }}>
            <div style={{ height: 10, background: 'var(--wf-head)', borderRadius: 2, marginBottom: 6 }} />
            <div style={{ height: 10, background: 'var(--wf-head)', borderRadius: 2, marginBottom: 6, width: '70%' }} />
            <div style={{ position: 'relative', height: STAFF, marginTop: 10 }}>
              <Sprite
                src={`${S}researcher_${researchLive && !reduced ? readFrame : 'idle'}.png`}
                left={18}
                top={0}
                w={STAFF}
                h={STAFF}
                z={2}
              />
            </div>
            <div style={{ fontSize: '0.62rem', color: 'var(--wf-dim)', textAlign: 'center', marginTop: 4 }}>
              {researchLive ? 'researching' : 'research'}
            </div>
          </div>

          {/* Auditor — corner; judges during the clean-build audit, else statue-still. */}
          <div style={{ position: 'absolute', right: 16, top: 12, width: 90, textAlign: 'center', zIndex: 2 }}>
            <Sprite
              src={`${S}auditor_judge_${auditLive && !reduced ? judgeFrame : 'a'}.png`}
              left={21}
              top={0}
              w={STAFF}
              h={STAFF}
              z={2}
            />
            <div style={{ fontSize: '0.62rem', color: auditLive ? 'var(--wf-fg)' : 'var(--wf-dim)', marginTop: STAFF + 2 }}>
              {auditLive ? 'auditing' : 'audit'}
            </div>
          </div>

          {/* PM — paces the bottom lane (drafting/ready) or stands by the wall board (running+). */}
          {!pmPacing && (
            <div
              style={{
                position: 'absolute', left: width - STAFF - 118, top: pmLaneY - 6, width: 44, height: 56,
                border: '1px solid var(--wf-border)', background: 'var(--wf-panel)', borderRadius: 2, zIndex: 1,
              }}
            />
          )}
          <Sprite src={`${S}${pmStep}.png`} left={pmX} top={pmLaneY} w={STAFF} h={STAFF} z={5} flip={pmFlip} />
          <div
            style={{
              position: 'absolute', left: pmX - 16, top: pmLaneY + STAFF, width: 80, textAlign: 'center',
              fontSize: '0.62rem', color: 'var(--wf-dim)', zIndex: 5, whiteSpace: 'nowrap',
            }}
          >
            {pmPacing
              ? 'front office'
              : isDraftingFamilyActivity(project?.officeActivity?.label)
                ? `PM · ${project.officeActivity!.label}`
                : 'PM · standup'}
          </div>
        </motion.div>
      </div>

      {/* Legend — chrome, --wf-* tokens only. */}
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: '1.2rem', color: 'var(--wf-dim)', fontSize: '0.7rem', padding: '0.6rem 0.2rem 0' }}>
        <Legend color="var(--wf-info)" label="working" note="monitor blinking, hands typing" />
        <Legend color="var(--wf-review)" label="in review" note="reviewer debating at the desk" />
        <Legend color="var(--wf-warn)" label="parked / waiting" note="blocked or no free chair" />
        <Legend color="var(--wf-dim)" label="capacity free" note="empty desk" />
      </div>
    </div>
  );
};

const Legend: React.FC<{ color: string; label: string; note: string }> = ({ color, label, note }) => (
  <span style={{ display: 'inline-flex', alignItems: 'baseline', gap: '0.4rem' }}>
    <span style={{ display: 'inline-block', width: 6, height: 6, borderRadius: '50%', background: color, transform: 'translateY(-1px)' }} />
    <b style={{ color: 'var(--wf-fg)', fontWeight: 600 }}>{label}</b>
    <span>{note}</span>
  </span>
);

export default OfficeMap;
