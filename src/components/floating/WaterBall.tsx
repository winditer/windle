import { cn } from "@/lib/utils";

/** The ball fills its window exactly; keep in step with `floating::BALL_SIZE`. */
const SIZE = 132;

/** One full wavelength per layer, in ball pixels — also the loop distance. */
const PERIOD_SLOW = 66;
const PERIOD_FAST = 88;

/** Wavelengths drawn per layer. The water has to stay under the glass while the
    swirl sweeps it along, and a ten-wavelength sheet can afford the travel. */
const PERIODS = 10;

/** How far below the waterline the body of the water is drawn. Far enough that
    the bottom edge of a filled path never reaches the glass. */
const DEPTH = SIZE * 6;

/** The vortex, as a share of the water's depth and a cap in ball pixels — a
    full ball would otherwise disappear under its own rings. */
const VORTEX_SPAN = 0.42;
const VORTEX_MAX = 46;

/** Memory use at or above this share is drawn as a warning. Compared against the
    rounded reading, so the colour always agrees with the number on screen. */
export const MEMORY_ALERT_PERCENT = 90;

/** The warning colour, shared by the ball's reading and the panel's memory row. */
export const MEMORY_ALERT_COLOR = "#f97316";

/**
 * A water surface with `period`-long waves, drawn `PERIODS` wavelengths wide
 * starting two wavelengths to the left, so sliding it by a period — the drift
 * loop — or by the swirl's whole sweep still leaves the view box covered. The
 * body reaches far past the bottom; the clip circle is what shapes it.
 */
function wavePath(period: number, crest: number): string {
  const half = period / 2;
  const steps = PERIODS * 2;
  const left = -3 * period;

  let path = `M${left},0 q${half / 2},${-crest} ${half},0`;
  for (let step = 1; step < steps; step += 1) {
    path += ` t${half},0`;
  }

  return `${path} V${DEPTH} H${left} Z`;
}

/**
 * A spiral of `turns` turns growing from `inner` to `outer`, walked as a
 * polyline about (0, 0). Water on its way to the middle of a swirl traces a
 * shape like this, which is what makes it read as going round rather than as a
 * ring sitting still.
 */
function spiralPath(inner: number, outer: number, turns: number): string {
  const steps = 72;
  let path = "";

  for (let step = 0; step <= steps; step += 1) {
    const along = step / steps;
    const angle = along * turns * Math.PI * 2;
    const radius = inner + (outer - inner) * along;
    const x = Math.cos(angle) * radius;
    const y = Math.sin(angle) * radius;
    path += `${step === 0 ? "M" : "L"}${x.toFixed(2)},${y.toFixed(2)}`;
  }

  return path;
}

interface WaterBallProps {
  /** Share of the ball filled with water, 0–1. */
  level: number;
  dragging?: boolean;
  /** A release is running: the water turns the way it does in a barrel being
      swirled, but looked at down the axis — the waterline stays level, and it
      is the middle of the water that goes round. */
  swirling?: boolean;
}

export function WaterBall({ level, dragging = false, swirling = false }: WaterBallProps) {
  const clamped = Math.min(1, Math.max(0, level));
  // The waves sit on the level line, so their crests reach a little above it —
  // which at an empty level would read as a puddle, so the water goes away.
  const surface = SIZE * (1 - clamped);
  const depth = SIZE - surface;

  // The vortex is drawn to the water it sits in: a full ball turns a wide
  // circle, a nearly empty one a small one. Its outer ring is a share of the
  // depth, so the whole of it stays inside the water at every level.
  const vortex = Math.min(VORTEX_MAX, depth * VORTEX_SPAN);
  const stroke = Math.max(3.5, vortex * 0.2);

  const percent = Math.round(clamped * 100);
  const alert = percent >= MEMORY_ALERT_PERCENT;

  return (
    <svg
      viewBox={`0 0 ${SIZE} ${SIZE}`}
      className={cn(
        "size-full transition-transform duration-150",
        dragging && "scale-105",
      )}
    >
      <defs>
        {/* Everything the water touches is clipped to the glass. */}
        <clipPath id="ball-clip">
          <circle cx={SIZE / 2} cy={SIZE / 2} r={SIZE / 2} />
        </clipPath>
        <linearGradient id="ball-water" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#4ade80" stopOpacity="0.82" />
          <stop offset="100%" stopColor="#15803d" stopOpacity="0.92" />
        </linearGradient>
        <radialGradient id="ball-vortex" cx="0.5" cy="0.5" r="0.5">
          <stop offset="0%" stopColor="#04120b" stopOpacity="0.9" />
          <stop offset="60%" stopColor="#04120b" stopOpacity="0.45" />
          <stop offset="100%" stopColor="#04120b" stopOpacity="0" />
        </radialGradient>
        <radialGradient id="ball-glass" cx="0.32" cy="0.24" r="0.8">
          <stop offset="0%" stopColor="#ffffff" stopOpacity="0.22" />
          <stop offset="55%" stopColor="#ffffff" stopOpacity="0.05" />
          <stop offset="100%" stopColor="#04120b" stopOpacity="0.42" />
        </radialGradient>
      </defs>

      {/* The dark glass reads on light and dark desktops alike. */}
      <circle cx={SIZE / 2} cy={SIZE / 2} r={SIZE / 2} fill="rgba(6,20,13,0.45)" />

      <g clipPath="url(#ball-clip)">
        <g
          className="transition-[transform,opacity] duration-700 ease-out"
          style={{ transform: `translateY(${surface}px)`, opacity: clamped > 0 ? 1 : 0 }}
        >
          {/* Both layers are swept together, so the racing water reads as one
              body turning rather than two sets of waves crossing. The sweep
              is `ball-swirl`'s, which is a whole number of wavelengths for
              each period (264 = 4 × 66 = 3 × 88) — that is what keeps the
              surface in the same phase at both ends of it. */}
          <g className={cn(swirling && "animate-ball-swirl")}>
            <path d={wavePath(PERIOD_FAST, 14)} fill="url(#ball-water)" opacity="0.55">
              <animateTransform
                attributeName="transform"
                type="translate"
                from="0,0"
                to={`${-PERIOD_FAST},0`}
                dur="11s"
                repeatCount="indefinite"
              />
            </path>
            <path d={wavePath(PERIOD_SLOW, 12)} fill="url(#ball-water)">
              <animateTransform
                attributeName="transform"
                type="translate"
                from="0,0"
                to={`${-PERIOD_SLOW},0`}
                dur="7s"
                repeatCount="indefinite"
              />
            </path>
          </g>

          {/* The vortex, looked at down its axis: a hole in the middle of the
              water with rings turning around it, the inner ones quicker, the
              way a real one turns. Every ring is drawn about its own parent's
              origin, so the turn is an attribute rather than an origin the
              engine has to agree on, and a whole number of turns per cycle
              leaves nothing to jump at either end.

              It exists only while a release runs: at rest there is no vortex
              to draw, and it fades in over the water and out again, so the
              mounting and unmounting cannot be seen. */}
          {swirling && (
            <g className="animate-ball-vortex">
              <g transform={`translate(${SIZE / 2}, ${depth / 2})`}>
                <circle r={vortex * 0.34} fill="url(#ball-vortex)" />
                <path
                  d={spiralPath(vortex * 0.34, vortex * 0.98, 1.7)}
                  fill="none"
                  stroke="#04120b"
                  strokeOpacity="0.42"
                  strokeWidth={stroke}
                  strokeLinecap="round"
                >
                  <animateTransform
                    attributeName="transform"
                    type="rotate"
                    from="0 0 0"
                    to="360 0 0"
                    dur="0.65s"
                    repeatCount="indefinite"
                  />
                </path>
                <circle
                  r={vortex * 0.62}
                  fill="none"
                  stroke="#04120b"
                  strokeOpacity="0.34"
                  strokeWidth={stroke * 0.8}
                  strokeLinecap="round"
                  strokeDasharray={`${vortex * 0.34} ${vortex * 0.25}`}
                >
                  <animateTransform
                    attributeName="transform"
                    type="rotate"
                    from="0 0 0"
                    to="360 0 0"
                    dur="0.5s"
                    repeatCount="indefinite"
                  />
                </circle>
                <circle
                  r={vortex}
                  fill="none"
                  stroke="#04120b"
                  strokeOpacity="0.28"
                  strokeWidth={stroke * 0.7}
                  strokeLinecap="round"
                  strokeDasharray={`${vortex * 0.55} ${vortex * 0.4}`}
                >
                  <animateTransform
                    attributeName="transform"
                    type="rotate"
                    from="0 0 0"
                    to="360 0 0"
                    dur="0.85s"
                    repeatCount="indefinite"
                  />
                </circle>
              </g>
            </g>
          )}
        </g>

        {/* Glass sheen over the water, so the ball still reads as a sphere. */}
        <circle cx={SIZE / 2} cy={SIZE / 2} r={SIZE / 2} fill="url(#ball-glass)" />
      </g>

      <circle
        cx={SIZE / 2}
        cy={SIZE / 2}
        r={SIZE / 2 - 1}
        fill="none"
        stroke="#ffffff"
        strokeOpacity={dragging ? 0.6 : 0.34}
        strokeWidth="1.5"
      />

      {/* The reading, over everything the ball is made of: white on the glass and
          on the water alike, with a dark edge so it holds up against both. */}
      <text
        x={SIZE / 2}
        y={SIZE / 2}
        textAnchor="middle"
        dominantBaseline="central"
        paintOrder="stroke"
        stroke="#04120b"
        strokeOpacity="0.5"
        strokeWidth="4"
        fill={alert ? MEMORY_ALERT_COLOR : "#ffffff"}
        className="font-sans font-semibold tabular-nums transition-colors duration-500"
        style={{ fontSize: 36 }}
      >
        {`${percent}%`}
      </text>
    </svg>
  );
}
