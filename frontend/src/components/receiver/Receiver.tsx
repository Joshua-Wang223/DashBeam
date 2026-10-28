import { ChevronDown, ChevronUp, Info } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useTranslation } from '../../i18n/react-i18next-compat'
import { IS_WEB } from '../../lib/platform'
import { PulseAnimation } from '../common/PulseAnimation'
import { TransferSuccessScreen } from '../common/TransferSuccessScreen'
import { Button } from '../ui/button'
import {
	Dialog,
	DialogClose,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '../ui/dialog'
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from '../ui/select'
import { Slider } from '../ui/slider'
import { useReceiverContext } from './ReceiverProvider'
import { ReceiveSaveLocationPicker } from './ReceiveSaveLocationPicker'
import { ReceivingActiveCard } from './ReceivingActiveCard'
import { TicketInput } from './TicketInput'

interface ReceiverProps {
	onTransferStateChange: (isReceiving: boolean) => void
}

export function Receiver({ onTransferStateChange }: ReceiverProps) {
	const [showInstructionsDialog, setShowInstructionsDialog] = useState(false)
	const { t } = useTranslation()

	const {
		ticket,
		isReceiving,
		isTransporting,
		isCompleted,
		savePath,
		alertDialog,
		transferMetadata,
		transferProgress,
		previewMetadata,
		isPreviewLoading,
		isExportPending,
		fileNames,
		splitStrategy,
		setSplitStrategy,
		maxConcurrency,
		setMaxConcurrency,
		showAdvancedOptions,
		setShowAdvancedOptions,
		handleTicketChange,
		handleBrowseFolder,
		handleReceive,
		handleOpenFolder,
		closeAlert,
		resetForNewTransfer,
	} = useReceiverContext()

	useEffect(() => {
		onTransferStateChange(isReceiving)
	}, [isReceiving, onTransferStateChange])

	return (
		<div className="p-2 sm:p-6 space-y-6 relative h-[62dvh] sm:h-112 overflow-y-auto flex flex-col">
			{!isReceiving ? (
				<>
					<div className="text-center">
						<div className="flex items-center justify-center gap-2 mb-2">
							<h2 className="text-xl font-semibold">
								{t('common:receiver.title')}
							</h2>
							<Button
								size="icon-sm"
								type="button"
								variant="ghost"
								onClick={() => setShowInstructionsDialog(true)}
								className="absolute top-0 right-0 sm:top-6 sm:right-6"
							>
								<Info />
							</Button>
						</div>
						<p className="text-sm text-muted-foreground">
							{t('common:receiver.subtitle')}
						</p>
					</div>

					<div className="space-y-4 flex-1 flex flex-col">
						<ReceiveSaveLocationPicker
							savePath={savePath}
							disabled={isReceiving}
							onBrowseFolder={handleBrowseFolder}
						/>
						<TicketInput
							ticket={ticket}
							isReceiving={isReceiving}
							previewMetadata={previewMetadata}
							isPreviewLoading={isPreviewLoading}
							onTicketChange={handleTicketChange}
							onReceive={handleReceive}
						/>

						{/* The web build always downloads sequentially (browser stream
						    limits), so there is nothing to configure there. */}
						{!IS_WEB && (
							<div className="mt-2">
								<button
									type="button"
									className="flex items-center gap-2 text-sm font-medium text-foreground hover:text-primary"
									onClick={() => setShowAdvancedOptions(!showAdvancedOptions)}
								>
									{showAdvancedOptions ? (
										<ChevronUp className="h-4 w-4" />
									) : (
										<ChevronDown className="h-4 w-4" />
									)}
									{t('common:receiver.advancedDownloadOptions')}
								</button>

								{showAdvancedOptions && (
									<div className="mt-3 space-y-4 p-4 border rounded-md bg-muted/20">
										<div className="space-y-2">
											<span className="text-sm font-medium">
												{t('common:receiver.downloadStrategy')}
											</span>
											<Select
												value={splitStrategy}
												onValueChange={(value) =>
													setSplitStrategy(value as 'none' | 'split')
												}
												disabled={isReceiving}
											>
												<SelectTrigger
													aria-label={t('common:receiver.downloadStrategy')}
												>
													<SelectValue
														placeholder={t('common:receiver.selectStrategy')}
													/>
												</SelectTrigger>
												<SelectContent>
													<SelectItem value="none">
														{t('common:receiver.strategySequential')}
													</SelectItem>
													<SelectItem value="split">
														{t('common:receiver.strategyParallel')}
													</SelectItem>
												</SelectContent>
											</Select>
											<p className="text-xs text-muted-foreground">
												{splitStrategy === 'split'
													? t('common:receiver.strategyParallelDesc')
													: t('common:receiver.strategySequentialDesc')}
											</p>
										</div>

										{splitStrategy === 'split' && (
											<div className="space-y-2">
												<div className="flex justify-between">
													<span className="text-sm font-medium">
														{maxConcurrency === null
															? t('common:receiver.maxConcurrencyAuto')
															: t('common:receiver.maxConcurrency', {
																	value: maxConcurrency,
																})}
													</span>
												</div>
												<Slider
													aria-label={t('common:receiver.maxConcurrencyAuto')}
													value={[maxConcurrency ?? 16]}
													onValueChange={(vals) =>
														setMaxConcurrency(
															Array.isArray(vals) ? vals[0] : vals
														)
													}
													min={2}
													max={32}
													step={1}
													disabled={isReceiving}
												/>
												<p className="text-xs text-muted-foreground">
													{t('common:receiver.maxConcurrencyDesc')}
												</p>
											</div>
										)}
									</div>
								)}
							</div>
						)}
					</div>
				</>
			) : isCompleted && transferMetadata ? (
				<div className="flex-1 flex flex-col">
					<TransferSuccessScreen
						metadata={transferMetadata}
						onDone={resetForNewTransfer}
						onOpenFolder={handleOpenFolder}
						isOpenPending={isExportPending}
					/>
				</div>
			) : (
				<>
					<div className="text-center">
						<PulseAnimation
							isTransporting={isTransporting}
							className="mx-auto my-4 flex items-center justify-center"
						/>
					</div>
					<div className="flex-1 flex flex-col">
						<ReceivingActiveCard
							isReceiving={isReceiving}
							isTransporting={isTransporting}
							isCompleted={isCompleted}
							ticket={ticket}
							transferProgress={transferProgress}
							fileNames={fileNames}
							onReceive={handleReceive}
							onStopReceiving={resetForNewTransfer}
						/>
					</div>
				</>
			)}

			<Dialog open={alertDialog.isOpen} onOpenChange={closeAlert}>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>{alertDialog.title}</DialogTitle>
						<DialogDescription>{alertDialog.description}</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<DialogClose
							onClick={closeAlert}
							render={<Button size="sm">{t('common:ok')}</Button>}
						/>
					</DialogFooter>
				</DialogContent>
			</Dialog>

			<Dialog
				open={showInstructionsDialog}
				onOpenChange={setShowInstructionsDialog}
			>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>{t('common:receiver.howToReceive')}</DialogTitle>
						<DialogDescription></DialogDescription>
						<ol className="text-sm space-y-2 list-decimal list-inside mt-2">
							<li>{t('common:receiver.instruction1')}</li>
							<li>{t('common:receiver.instruction2')}</li>
							<li>{t('common:receiver.instruction3')}</li>
							<li>{t('common:receiver.instruction4')}</li>
							<li>{t('common:receiver.instruction5')}</li>
						</ol>
					</DialogHeader>
					<DialogFooter>
						<DialogClose
							render={<Button size="sm">{t('common:ok')}</Button>}
							onClick={() => setShowInstructionsDialog(false)}
						/>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</div>
	)
}
