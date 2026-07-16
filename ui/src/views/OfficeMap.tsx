import React, { useEffect, useMemo, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import type { Project } from './Board';
import {
  DESK,
  TABLE,
  PersonaPresence,
  idleAssignmentsFor,
  idlePersonasFor,
  isAuditLive,
  isDraftingFamilyActivity,
  isResearchLive,
  isSprintReview,
  isWaitingOnUserActivity,
  clampMaxWorkers,
  meetingSeatsFor,
  occupiedCount,
  presenceFor,
  reviewedSprint,
  sprintAttendees,
  sprintBadgeText,
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

  // Sprint-review meeting room (feature: sprints): while the current sprint is InReview, the
  // desk grid swaps for a table scene and the ceremony transcript replays as chat bubbles.
  // Absent `activeSprint`/`sprints` (pre-sprints / no-sprint-track snapshot) -> `false`/`null`
  // throughout, so the classic desk-grid office renders unchanged (back-compat).
  const sprintReview = isSprintReview(project as any);
  const reviewed = useMemo(() => reviewedSprint(project as any), [project]);
  const attendees = useMemo(() => sprintAttendees(project as any, reviewed), [project, reviewed]);
  const seats = useMemo(() => meetingSeatsFor(attendees), [attendees]);
  const transcript = reviewed?.transcript ?? [];
  // ~2.5s/line (spec) at the office's 200ms tick -> 13 ticks/line (2.6s). Derived from the same
  // single `tick` clock as every other frame here, so replaying is loop-safe on re-render/remount
  // (no local mutable line-index state to fall out of sync) and pauses whenever `tick` does
  // (reduced motion, or the office tab unmounting because it isn't the active tab).
  const LINE_TICKS = 13;
  const lineIdx = transcript.length > 0 ? Math.floor(tick / LINE_TICKS) % transcript.length : 0;
  const currentLine = transcript[lineIdx];
  const sprintBadge = sprintBadgeText(project as any);
  const researcherSpeaking = sprintReview && currentLine?.speaker === 'researcher';

  // Ambient idle life (feature: ambient-idle-life): personas with no active task (and, during a
  // review, not seated at the meeting table either) wander/gossip/cooler instead of standing
  // frozen. Derived from the same `presence`/`tick` this file already computes for everything
  // else — no extra timers, so it's free while the office tab is hidden (OfficeMap unmounts when
  // Board.tsx switches tabs, which clears `useTick`'s interval) and inert when reduced motion
  // freezes `tick` at 0.
  const idlePersonas = idlePersonasFor(presence, sprintReview ? attendees : []);
  const idleAssignments = useMemo(() => idleAssignmentsFor(idlePersonas, tick), [idlePersonas, tick]);

  // Derived animation frames (all from the single tick).
  const typeFrame = Math.floor(tick / 2) % 2 === 0 ? 'type_a' : 'type_b';
  const blinkOn = tick % 7 !== 6; // occasional dark flicker reads as screen activity
  const blinkFrame = Math.floor(tick / 2) % 2 === 0 ? 'a' : 'b';
  const beat = Math.floor(tick / 4); // ~800ms debate beat
  const readFrame = Math.floor(tick / 3) % 2 === 0 ? 'reading_a' : 'reading_b';
  const judgeFrame = Math.floor(tick / 3) % 2 === 0 ? 'a' : 'b';

  // Container bounds: desks grid (or, during a review, the meeting table's seat rows — whichever
  // is taller) + an ambient-idle-life lane + a bottom lane for the PM + side margins for the
  // fixed staff.
  const cols = desks.reduce((m, d) => Math.max(m, d.x), 0);
  const rows = desks.reduce((m, d) => Math.max(m, d.y), 0);
  const deskContentBottom = rows + DESK.cellH + DESK.padY;
  const meetingRows = Math.max(1, Math.ceil(attendees.length / TABLE.cols));
  const meetingContentBottom = TABLE.padY + TABLE.seatH * (meetingRows + 1) + 40;
  const contentBottom = sprintReview ? Math.max(deskContentBottom, meetingContentBottom) : deskContentBottom;
  const IDLE_LANE_H = 64; // ambient idle-life band, between the desks/table area and the PM lane
  const width = Math.max(cols + DESK.cellW + DESK.padX, 520) + 120; // +120 left bookshelf lane
  const height = contentBottom + IDLE_LANE_H + 120; // +120 bottom PM lane
  const floorLeft = 120; // desks + staff shift right of the bookshelf lane
  const idleLaneY = contentBottom + 14;
  const coolerX = floorLeft + 20;
  const commonsX = floorLeft + 130;
  const wanderLeft = floorLeft + 230;
  const wanderRight = Math.max(wanderLeft + 100, width - 140);

  const phaseKind = project?.phase?.kind;
  const pmPacing = phaseKind === 'drafting' || phaseKind === 'ready';
  const researchLive = isResearchLive(project as any);
  const auditLive = isAuditLive(project as any);
  // Safeguard feature 5: the drafting pipeline is stopped on pending assumptions — the PM stands
  // at the front office with a "?" over their head, waiting on the user.
  const waitingOnUser = isWaitingOnUserActivity(project?.officeActivity?.label);

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
      {/* Sprint badge (feature: sprints): "sprint i/N — goal", near the office view's own header
          area. Absent `activeSprint` (pre-sprints / no-sprint-track snapshot) renders nothing
          (back-compat). */}
      {sprintBadge && (
        <div
          data-testid="sprint-badge"
          style={{ fontSize: '0.72rem', color: 'var(--wf-fg)', padding: '0 0.2rem 0.5rem' }}
        >
          <b>{sprintBadge}</b>
          {sprintReview && <span style={{ color: 'var(--wf-review)' }}> — in review</span>}
        </div>
      )}
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

          {/* Desks: occupied personas packed first, remaining desks empty ("capacity free").
              Swaps for the meeting-room scene below while the current sprint is InReview. */}
          {!sprintReview && (
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
          )}

          {/* Meeting room (feature: sprints): the current sprint's review ceremony. Persona
              sprites gather around a table (reusing desk.png/chair.png at a bigger scale, the
              same pixel-art grammar as the desk cells above) — the PM and reviewer anchor the
              head, workers who worked the sprint's tasks ring the remaining seats. The
              researcher keeps their existing bookshelf-lane spot (rendered below) rather than
              moving to the table — that IS their "corner observer spot". The ceremony
              transcript replays one line at a time (`currentLine`, derived from `tick`); the
              speaking seat is highlighted with a bubble over their sprite. */}
          {sprintReview && (
            <React.Fragment>
              <Sprite
                src={`${S}desk.png`}
                left={floorLeft + TABLE.padX - 20}
                top={TABLE.padY + 40}
                w={TABLE.seatW * 2 + 40}
                h={110}
                z={1}
              />
              {seats.map((seat, i) => {
                const isPm = seat.role === 'pm';
                const isReviewer = seat.role === 'reviewer';
                const persona = seat.persona;
                const speaking = Boolean(
                  currentLine &&
                    ((isPm && currentLine.speaker === 'office') ||
                      (isReviewer && currentLine.speaker === 'reviewer') ||
                      (seat.role === 'worker' && currentLine.speaker === persona)),
                );
                const workerTurn = beat % 2 === 0;
                const label = isPm ? 'PM' : isReviewer ? 'reviewer' : (persona ?? '');

                return (
                  <div
                    key={`meeting-seat-${seat.role}-${persona ?? i}`}
                    data-testid="meeting-seat"
                    data-role={seat.role}
                    data-persona={persona ?? ''}
                    data-speaking={speaking ? 'true' : 'false'}
                    style={{
                      position: 'absolute',
                      left: floorLeft + seat.x,
                      top: seat.y,
                      width: WORKER,
                      height: WORKER + 46,
                      zIndex: 2,
                      boxShadow: speaking ? 'inset 0 0 0 2px var(--wf-accent)' : 'none',
                      borderRadius: 6,
                    }}
                  >
                    <Sprite src={`${S}chair.png`} left={0} top={40} w={CHAIR} h={CHAIR} z={1} />
                    {isPm && (
                      <Sprite
                        src={`${S}${tick % 2 === 0 ? 'pm_walk_a' : 'pm_walk_b'}.png`}
                        left={0}
                        top={0}
                        w={STAFF}
                        h={STAFF}
                        z={2}
                      />
                    )}
                    {isReviewer && (
                      <Sprite
                        src={`${S}reviewer_point_${workerTurn ? 'a' : 'b'}.png`}
                        left={0}
                        top={0}
                        w={WORKER}
                        h={WORKER}
                        z={2}
                      />
                    )}
                    {seat.role === 'worker' && persona && (
                      <Sprite src={`${S}worker_${persona}_idle.png`} left={0} top={0} w={WORKER} h={WORKER} z={2} />
                    )}

                    {speaking && !reduced && currentLine && (
                      <div
                        data-testid="meeting-bubble"
                        style={{
                          position: 'absolute',
                          top: -36,
                          left: -24,
                          minWidth: 96,
                          maxWidth: 170,
                          background: 'var(--wf-panel)',
                          border: '1px solid var(--wf-border)',
                          borderRadius: 'var(--wf-radius)',
                          padding: '0.2rem 0.4rem',
                          fontSize: '0.62rem',
                          color: 'var(--wf-fg)',
                          zIndex: 7,
                        }}
                      >
                        {currentLine.text}
                      </div>
                    )}

                    <div
                      style={{
                        position: 'absolute',
                        top: WORKER + 10,
                        left: -24,
                        width: WORKER + 48,
                        textAlign: 'center',
                        fontSize: '0.62rem',
                        color: speaking ? 'var(--wf-fg)' : 'var(--wf-dim)',
                        whiteSpace: 'nowrap',
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                      }}
                    >
                      {label}
                    </div>
                  </div>
                );
              })}
            </React.Fragment>
          )}

          {/* Ambient idle life (feature: ambient-idle-life): personas with no active task (and,
              during a review, not seated at the meeting table) wander the floor, gossip-pair at
              the empty commons table, or visit the water cooler, instead of standing frozen.
              Positions are deterministic per `(idlePersonas, tick)` (`idleAssignmentsFor` — see
              officeLayout.ts, no `Math.random`), so this is reproducible frame-to-frame. */}
          <React.Fragment>
            {/* Water cooler — a fixture on the map like the researcher's bookshelf, present even
                when nobody is visiting it. */}
            <div
              data-testid="water-cooler"
              style={{
                position: 'absolute',
                left: coolerX - 2,
                top: idleLaneY - 30,
                width: 22,
                height: 34,
                background: 'var(--wf-panel)',
                border: '1px solid var(--wf-border)',
                borderRadius: 3,
                zIndex: 1,
              }}
            >
              <div style={{ height: 9, background: 'var(--wf-info)', borderRadius: '2px 2px 0 0' }} />
            </div>
            <div
              style={{
                position: 'absolute',
                left: coolerX - 30,
                top: idleLaneY + 6,
                width: 80,
                textAlign: 'center',
                fontSize: '0.58rem',
                color: 'var(--wf-dim)',
                zIndex: 1,
              }}
            >
              water cooler
            </div>

            {/* Commons table — an empty desk + two chairs, the gossip spot. */}
            <Sprite src={`${S}desk.png`} left={commonsX - 4} top={idleLaneY - 4} w={DESK_W} h={DESK_H} z={1} />
            <Sprite src={`${S}chair.png`} left={commonsX - 30} top={idleLaneY - 2} w={36} h={36} z={1} />
            <Sprite src={`${S}chair.png`} left={commonsX + 62} top={idleLaneY - 2} w={36} h={36} z={1} flip />

            {idleAssignments.map((a) => {
              let left: number;
              let flip = false;
              if (a.activity === 'cooler') {
                left = coolerX + 10;
              } else if (a.activity === 'gossip') {
                const isLeftSeat = a.partner !== undefined && a.persona < a.partner;
                left = isLeftSeat ? commonsX - 14 : commonsX + 46;
                flip = !isLeftSeat;
              } else {
                const wA = a.waypointA ?? 0;
                const wB = a.waypointB ?? 0;
                const frac = wA + (wB - wA) * a.t;
                left = wanderLeft + frac * (wanderRight - wanderLeft);
                flip = wB < wA;
              }
              return (
                <div
                  key={`idle-${a.persona}`}
                  data-testid="idle-sprite"
                  data-persona={a.persona}
                  data-activity={a.activity}
                  style={{ position: 'absolute', left, top: idleLaneY, width: WORKER, height: WORKER + 20, zIndex: 2 }}
                >
                  <Sprite src={`${S}worker_${a.persona}_idle.png`} left={0} top={0} w={WORKER} h={WORKER} z={2} flip={flip} />
                  {a.bubble && !reduced && (
                    <div
                      data-testid="idle-bubble"
                      style={{
                        position: 'absolute',
                        top: -26,
                        left: -16,
                        minWidth: 40,
                        background: 'var(--wf-panel)',
                        border: '1px solid var(--wf-border)',
                        borderRadius: 'var(--wf-radius)',
                        padding: '0.1rem 0.35rem',
                        fontSize: '0.6rem',
                        color: 'var(--wf-fg)',
                        zIndex: 7,
                        whiteSpace: 'nowrap',
                      }}
                    >
                      {a.bubble}
                    </div>
                  )}
                </div>
              );
            })}
          </React.Fragment>

          {/* Researcher — bookshelf lane; reads while research is in flight, else stands idle.
              During a sprint review (feature: sprints) this same bookshelf spot doubles as their
              "corner observer spot" — the researcher never moves to the meeting table, only their
              label/highlight changes, and their transcript line (always present —
              `assemble_sprint_transcript` always pushes a researcher line) replays here too. */}
          <div style={{ position: 'absolute', left: 18, top: 60, width: 84, zIndex: 2 }}>
            <div style={{ height: 10, background: 'var(--wf-head)', borderRadius: 2, marginBottom: 6 }} />
            <div style={{ height: 10, background: 'var(--wf-head)', borderRadius: 2, marginBottom: 6, width: '70%' }} />
            <div
              data-testid="meeting-seat"
              data-role="researcher"
              data-speaking={researcherSpeaking ? 'true' : 'false'}
              style={{
                position: 'relative',
                height: STAFF,
                marginTop: 10,
                boxShadow: researcherSpeaking ? 'inset 0 0 0 2px var(--wf-accent)' : 'none',
                borderRadius: 6,
              }}
            >
              <Sprite
                src={`${S}researcher_${researchLive && !reduced ? readFrame : 'idle'}.png`}
                left={18}
                top={0}
                w={STAFF}
                h={STAFF}
                z={2}
              />
              {researcherSpeaking && !reduced && currentLine && (
                <div
                  data-testid="meeting-bubble"
                  style={{
                    position: 'absolute',
                    top: -34,
                    left: -6,
                    minWidth: 96,
                    maxWidth: 170,
                    background: 'var(--wf-panel)',
                    border: '1px solid var(--wf-border)',
                    borderRadius: 'var(--wf-radius)',
                    padding: '0.2rem 0.4rem',
                    fontSize: '0.62rem',
                    color: 'var(--wf-fg)',
                    zIndex: 7,
                  }}
                >
                  {currentLine.text}
                </div>
              )}
            </div>
            <div
              style={{
                fontSize: '0.62rem',
                color: researcherSpeaking ? 'var(--wf-fg)' : 'var(--wf-dim)',
                textAlign: 'center',
                marginTop: 4,
              }}
            >
              {sprintReview ? 'observing' : researchLive ? 'researching' : 'research'}
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

          {/* PM — paces the bottom lane (drafting/ready) or stands by the wall board (running+).
              During a sprint review (feature: sprints) the PM sits at the meeting table's head
              instead (rendered in the meeting-room block above) — this front-office figure is
              suppressed for the duration so there aren't two PMs on screen at once. */}
          {!sprintReview && (
            <React.Fragment>
              {!pmPacing && (
                <div
                  style={{
                    position: 'absolute', left: width - STAFF - 118, top: pmLaneY - 6, width: 44, height: 56,
                    border: '1px solid var(--wf-border)', background: 'var(--wf-panel)', borderRadius: 2, zIndex: 1,
                  }}
                />
              )}
              <Sprite src={`${S}${pmStep}.png`} left={pmX} top={pmLaneY} w={STAFF} h={STAFF} z={5} flip={pmFlip} />
              {/* Waiting on the user (pending assumptions): a "?" bubble above the PM. */}
              {waitingOnUser && (
                <img
                  src={`${S}bubble_q.png`}
                  alt=""
                  aria-hidden
                  data-testid="pm-waiting-bubble"
                  style={{
                    position: 'absolute', left: pmX + 14, top: pmLaneY - 22, width: BUB_W, height: BUB_H,
                    zIndex: 6, imageRendering: 'pixelated', pointerEvents: 'none',
                  }}
                />
              )}
              <div
                style={{
                  position: 'absolute', left: pmX - 16, top: pmLaneY + STAFF, width: 80, textAlign: 'center',
                  fontSize: '0.62rem', color: waitingOnUser ? 'var(--wf-warn)' : 'var(--wf-dim)', zIndex: 5, whiteSpace: 'nowrap',
                }}
              >
                {waitingOnUser
                  ? 'front office - waiting on you'
                  : pmPacing
                    ? 'front office'
                    : isDraftingFamilyActivity(project?.officeActivity?.label)
                      ? `PM · ${project.officeActivity!.label}`
                      : 'PM · standup'}
              </div>
            </React.Fragment>
          )}
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
