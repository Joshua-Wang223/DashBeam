import { useTranslation } from '../../i18n/react-i18next-compat'
import { formatFileSize } from '../../lib/utils'
import type { TransferProgress } from '../../types/transfer'
import { formatETA } from '../../utils/etaUtils'

/** BLAKE3 chunk size — the addressing unit the engine transfers in. */
const TRANSFER_CHUNK_BYTES = 1024

const compactNumber = new Intl.NumberFormat(undefined, {
	notation: 'compact',
	maximumFractionDigits: 1,
})

interface TransferProgressBarProps {
	progress: TransferProgress
}

export function formatSpeed(speedBps: number): string {
	const mbps = speedBps / (1024 * 1024)
	const kbps = speedBps / 1024
	if (mbps >= 1) {
		return `${mbps.toFixed(2)} MB/s`
	} else {
		return `${kbps.toFixed(2)} KB/s`
	}
}

const SEGMENT_COUNT = 18
const SEGMENT_KEYS = Array.from(
	{ length: SEGMENT_COUNT },
	(_, i) => `segment-${i}`
)
const SEGMENT_ANGLE = 360 / SEGMENT_COUNT
const GAP_ANGLE = 2.5
const ARC_ANGLE = SEGMENT_ANGLE - GAP_ANGLE * 2

const RING_SIZE = 200
const CENTER = RING_SIZE / 2
const RADIUS = 84
const STROKE_WIDTH = 4.5

function polarToCartesian(cx: number, cy: number, r: number, angleDeg: number) {
	const angleRad = ((angleDeg - 90) * Math.PI) / 180
	return {
		x: cx + r * Math.cos(angleRad),
		y: cy + r * Math.sin(angleRad),
	}
}

function arcPath(startAngle: number, sweep: number): string {
	const start = polarToCartesian(CENTER, CENTER, RADIUS, startAngle)
	const end = polarToCartesian(CENTER, CENTER, RADIUS, startAngle + sweep)
	const largeArc = sweep > 180 ? 1 : 0
	return `M ${start.x} ${start.y} A ${RADIUS} ${RADIUS} 0 ${largeArc} 1 ${end.x} ${end.y}`
}

interface CircularRingProps {
	percentage: number
}

function CircularRing({ percentage }: CircularRingProps) {
	const { t } = useTranslation()
	const filledSegments = Math.floor((percentage / 100) * SEGMENT_COUNT)

	return (
		<svg
			viewBox={`0 0 ${RING_SIZE} ${RING_SIZE}`}
			width={RING_SIZE}
			height={RING_SIZE}
			role="progressbar"
			aria-label={t('common:transfer.progress')}
			aria-valuenow={Math.round(percentage)}
			aria-valuemin={0}
			aria-valuemax={100}
			className="mx-auto block"
		>
			{SEGMENT_KEYS.map((segmentKey, index) => {
				const segmentStartAngle = index * SEGMENT_ANGLE + GAP_ANGLE
				const isFilled = index < filledSegments
				const isPartiallyFilled =
					index === filledSegments && percentage % (100 / SEGMENT_COUNT) > 0

				let fillFraction = 0
				if (isFilled) {
					fillFraction = 1
				} else if (isPartiallyFilled) {
					fillFraction =
						(percentage % (100 / SEGMENT_COUNT)) / (100 / SEGMENT_COUNT)
				}

				const arcD = arcPath(segmentStartAngle, ARC_ANGLE)

				return (
					<g key={segmentKey}>
						<path
							d={arcD}
							fill="none"
							stroke="var(--input)"
							strokeWidth={STROKE_WIDTH}
							strokeLinecap="butt"
						/>
						{isFilled && (
							<path
								d={arcD}
								fill="none"
								stroke="var(--app-primary)"
								strokeWidth={STROKE_WIDTH}
								strokeLinecap="butt"
							/>
						)}
						{isPartiallyFilled && (
							<path
								d={arcD}
								fill="none"
								stroke="var(--app-primary)"
								strokeWidth={STROKE_WIDTH}
								strokeLinecap="butt"
								pathLength={100}
								strokeDasharray="100"
								strokeDashoffset={100 - fillFraction * 100}
								className="transition-[stroke-dashoffset] duration-300 ease-in-out"
							/>
						)}
					</g>
				)
			})}
		</svg>
	)
}

const BAR_COUNT = 30
const BAR_KEYS = Array.from({ length: BAR_COUNT }, (_, i) => `bar-${i}`)

export function TransferProgressBar({ progress }: TransferProgressBarProps) {
	const { percentage } = progress
	const barCount = BAR_COUNT
	const { t } = useTranslation()
	const filledBars = Math.floor((percentage / 100) * barCount)

	const chunkSizeLabel = formatFileSize(TRANSFER_CHUNK_BYTES, {
		precision: 0,
		smallPrecision: 0,
	})
	const totalChunks =
		progress.totalBytes > 0
			? Math.ceil(progress.totalBytes / TRANSFER_CHUNK_BYTES)
			: 0
	const chunksTransferred = Math.min(
		Math.floor(progress.bytesTransferred / TRANSFER_CHUNK_BYTES),
		totalChunks
	)
	const etaLabel =
		progress.etaSeconds !== undefined
			? formatETA(progress.etaSeconds)
			: t('common:transfer.calculating')
	const concurrency = progress.concurrency ?? 1

	return (
		<div className="space-y-3">
			<div className="sm:hidden flex flex-col items-center gap-3">
				<div className="relative inline-flex items-center justify-center">
					<CircularRing percentage={percentage} />

					<div className="absolute inset-0 flex flex-col items-center justify-center gap-0.5 text-center pointer-events-none">
						<span className="text-2xl font-normal leading-none tabular-nums">
							{percentage.toFixed(1)}%
						</span>
						<span className="text-xs text-muted-foreground">
							{formatSpeed(progress.speedBps)}
						</span>
						<span className="text-xs text-muted-foreground tabular-nums">
							{(progress.bytesTransferred / (1024 * 1024)).toFixed(2)} /{' '}
							{(progress.totalBytes / (1024 * 1024)).toFixed(2)} MB
						</span>
					</div>
				</div>
			</div>

			<div className="hidden sm:block space-y-2">
				<div className="flex items-center justify-between text-xs">
					<span>{t('common:transfer.progress')}</span>
					<span>{percentage.toFixed(1)}%</span>
				</div>

				<div className="flex gap-1 items-end h-8">
					{BAR_KEYS.map((barKey, index) => {
						const isFilled = index < filledBars
						const isPartiallyFilled =
							index === filledBars && percentage % (100 / barCount) > 0

						let fillPercentage = 100
						if (isPartiallyFilled) {
							const barProgress =
								(percentage % (100 / barCount)) / (100 / barCount)
							fillPercentage = barProgress * 100
						} else if (!isFilled) {
							fillPercentage = 0
						}

						return (
							<div
								key={barKey}
								className="relative flex-1 rounded-sm bg-input transition-all duration-300 ease-in-out"
								style={{ minWidth: '3px', height: '100%' }}
							>
								<div
									className="absolute bottom-0 left-0 right-0 rounded-sm transition-all duration-300 ease-in-out"
									style={{
										backgroundColor: 'var(--app-primary)',
										height: `${fillPercentage}%`,
									}}
								/>
							</div>
						)
					})}
				</div>

				<div className="flex items-center justify-between text-xs text-muted-foreground">
					<span>
						{t('common:transfer.speed')}: {formatSpeed(progress.speedBps)}
					</span>
					<span>
						{(progress.bytesTransferred / (1024 * 1024)).toFixed(2)} MB /{' '}
						{(progress.totalBytes / (1024 * 1024)).toFixed(2)} MB
					</span>
				</div>
			</div>

			<div className="grid grid-cols-3 gap-2 sm:gap-4">
				<TelemetryItem
					label={t('common:transfer.eta')}
					value={etaLabel}
					title={etaLabel}
				/>
				<TelemetryItem
					label={t('common:transfer.chunks', { size: chunkSizeLabel })}
					value={`${compactNumber.format(chunksTransferred)} / ${compactNumber.format(totalChunks)}`}
					title={`${chunksTransferred.toLocaleString()} / ${totalChunks.toLocaleString()}`}
				/>
				<TelemetryItem
					label={t('common:transfer.concurrency')}
					value={String(concurrency)}
				/>
			</div>
		</div>
	)
}

interface TelemetryItemProps {
	label: string
	value: string
	title?: string
}

function TelemetryItem({ label, value, title }: TelemetryItemProps) {
	return (
		<div className="flex min-w-0 flex-col gap-0.5" title={title}>
			<span className="truncate text-[10px] uppercase tracking-wide text-muted-foreground/70">
				{label}
			</span>
			<span className="truncate text-xs font-medium tabular-nums text-foreground/80">
				{value}
			</span>
		</div>
	)
}
