import { useEffect, useRef, useState } from 'react'
import { useTranslation } from '@/i18n'
import { shortFingerprint } from '@/lib/fingerprint'
import {
	formatDeviceTypeLabel,
	isKnownPairedEndpoint,
	respondPairRequest,
} from '@/lib/pairing-api'
import { IS_PAIRING_CAPABLE } from '@/lib/platform'
import { listen } from '@/lib/platform-api'
import {
	pairingDataHydrated,
	usePairingDataStore,
} from '@/store/pairing-data-store'
import {
	AlertDialog,
	AlertDialogContent,
	AlertDialogDescription,
	AlertDialogFooter,
	AlertDialogHeader,
	AlertDialogTitle,
} from '../ui/alert-dialog'
import { Button } from '../ui/button'
import { VerificationCode } from './VerificationCode'

export type NearbyPairRequestPayload = {
	remote_endpoint_id: string
	session_id: string
	sender_name: string
	device_type: string
	os?: string
}

/**
 * Receiver prompt for a dedicated nearby pairing request (no files).
 * Accept only records the peer as paired via `respond_nearby_invite`.
 */
export function NearbyPairRequestDialog() {
	const { t } = useTranslation()
	const [request, setRequest] = useState<NearbyPairRequestPayload | null>(null)
	const [submitting, setSubmitting] = useState(false)
	const [error, setError] = useState<string | null>(null)
	/** Suppresses the controlled dialog close event after a programmatic close. */
	const handledRef = useRef(false)

	useEffect(() => {
		if (!IS_PAIRING_CAPABLE) return

		let disposed = false
		let unlisten: (() => void) | undefined

		const setup = async () => {
			const stop = await listen(
				'nearby-pair-request-received',
				(event: { payload: unknown }) => {
					let payload: NearbyPairRequestPayload
					try {
						payload = JSON.parse(
							String(event.payload)
						) as NearbyPairRequestPayload
					} catch {
						return
					}
					void (async () => {
						await pairingDataHydrated()
						if (disposed) return
						const { devices } = usePairingDataStore.getState()
						if (isKnownPairedEndpoint(devices, payload.remote_endpoint_id)) {
							return
						}
						handledRef.current = false
						setError(null)
						setSubmitting(false)
						setRequest(payload)
					})()
				}
			)
			if (disposed) {
				stop()
			} else {
				unlisten = stop
			}
		}

		void setup()

		return () => {
			disposed = true
			unlisten?.()
		}
	}, [])

	if (!IS_PAIRING_CAPABLE) return null

	const decline = async () => {
		const current = request
		if (!current || submitting) return
		handledRef.current = true
		setSubmitting(true)
		setError(null)
		try {
			await respondPairRequest(
				current.remote_endpoint_id,
				current.session_id ?? '',
				false
			)
			setRequest(null)
		} catch (err) {
			handledRef.current = false
			setError(String(err))
		} finally {
			setSubmitting(false)
		}
	}

	const accept = async () => {
		const current = request
		if (!current || submitting) return
		handledRef.current = true
		setSubmitting(true)
		setError(null)
		try {
			await respondPairRequest(
				current.remote_endpoint_id,
				current.session_id ?? '',
				true
			)
			setRequest(null)
		} catch (err) {
			// Keep the dialog open so the user can retry; a silent failure here
			// used to leave the sender paired and this device not.
			handledRef.current = false
			setError(String(err))
		} finally {
			setSubmitting(false)
		}
	}

	const typeLabel = request ? formatDeviceTypeLabel(request.device_type) : ''
	// Pairing grants persistent access, so it gets at least the confirmation a
	// one-off file invite does. Display only — Accept stays enabled.
	const fingerprint = request
		? shortFingerprint(request.remote_endpoint_id)
		: null

	return (
		<AlertDialog
			open={request != null}
			onOpenChange={(open) => {
				if (!open && !submitting && !handledRef.current) void decline()
			}}
		>
			<AlertDialogContent>
				<AlertDialogHeader>
					<AlertDialogTitle>
						{t('common:receiver.nearbyPairRequest.title')}
					</AlertDialogTitle>
					<AlertDialogDescription>
						{request
							? typeLabel
								? t('common:receiver.nearbyPairRequest.descriptionWithType', {
										sender: request.sender_name,
										deviceType: typeLabel,
									})
								: t('common:receiver.nearbyPairRequest.description', {
										sender: request.sender_name,
									})
							: ''}
					</AlertDialogDescription>
				</AlertDialogHeader>
				{error ? (
					<p className="px-6 pb-2 text-sm text-destructive" role="alert">
						{t('common:receiver.pairFailed')}: {error}
					</p>
				) : null}
				<div className="px-6 pb-4">
					{/* Guarded on `request`: the popup stays mounted through its close
					    animation, so an unguarded render flashes the "no code" error
					    for that frame once Accept has cleared the request. */}
					{request ? (
						<VerificationCode
							code={fingerprint}
							hint={t('common:receiver.nearbyInvite.fingerprintHint', {
								sender: request.sender_name,
							})}
							unavailable={t(
								'common:receiver.nearbyInvite.fingerprintUnavailable'
							)}
						/>
					) : null}
				</div>
				<AlertDialogFooter>
					<Button
						size="sm"
						variant="outline"
						onClick={decline}
						disabled={submitting}
					>
						{t('common:receiver.declineInvite')}
					</Button>
					<Button size="sm" onClick={accept} disabled={submitting}>
						{t('common:receiver.acceptInvite')}
					</Button>
				</AlertDialogFooter>
			</AlertDialogContent>
		</AlertDialog>
	)
}
