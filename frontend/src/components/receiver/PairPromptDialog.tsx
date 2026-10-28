import { useEffect, useRef, useState } from 'react'

import { useTranslation } from '@/i18n'
import { Button } from '../ui/button'
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '../ui/dialog'
import { useReceiverContext } from './ReceiverProvider'

/**
 * Global post-transfer pairing prompt.
 *
 * Lives above the routed screens (inside `ReceiverProvider`) so a prompt that
 * arrives while the user is on Settings or Devices is not lost. Pairing is
 * asynchronous: this dialog only disables itself while the request is being
 * delivered; the actual result arrives as `device-paired`.
 */
export function PairPromptDialog() {
	const { t } = useTranslation()
	const { pairPrompt, acceptPairPrompt, dismissPairPrompt } =
		useReceiverContext()
	const [submitting, setSubmitting] = useState(false)
	const [error, setError] = useState<string | null>(null)
	/** Suppresses the controlled Dialog's close event after a programmatic close. */
	const handledRef = useRef(false)

	// A new sender (or a retry after dismissal) starts clean.
	useEffect(() => {
		if (!pairPrompt) return
		handledRef.current = false
		setError(null)
		setSubmitting(false)
	}, [pairPrompt])

	if (!pairPrompt) return null

	const accept = async () => {
		// `acceptPairPrompt` clears the prompt on success; mark this as a
		// programmatic close so onOpenChange does not record a dismissal.
		handledRef.current = true
		setSubmitting(true)
		setError(null)
		try {
			// The hook keeps the prompt open when the request cannot be
			// delivered, so the user can retry.
			const sent = await acceptPairPrompt()
			if (!sent) {
				handledRef.current = false
				setError(t('common:receiver.pairFailed'))
			}
		} finally {
			setSubmitting(false)
		}
	}

	const dismiss = () => {
		handledRef.current = true
		dismissPairPrompt()
	}

	return (
		<Dialog
			open
			onOpenChange={(open) => {
				if (!open && !submitting && !handledRef.current) dismiss()
			}}
		>
			<DialogContent>
				<DialogHeader>
					<DialogTitle>{t('common:receiver.pairPromptTitle')}</DialogTitle>
					<DialogDescription>
						{t('common:receiver.pairPromptDescription', {
							name: pairPrompt.displayName,
						})}
					</DialogDescription>
				</DialogHeader>
				{error ? (
					<p className="text-sm text-destructive" role="alert">
						{error}
					</p>
				) : null}
				<DialogFooter>
					<Button
						variant="outline"
						size="sm"
						onClick={dismiss}
						disabled={submitting}
					>
						{t('common:receiver.pairPromptDecline')}
					</Button>
					<Button size="sm" onClick={accept} disabled={submitting}>
						{t('common:receiver.pairPromptAccept')}
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	)
}
