<script lang="ts">
	import { tick } from 'svelte';
	import { invalidateMusicBrainzProviderQueries } from '$lib/queries/QueryClient';
	import {
		activateBrainzMash,
		consentBrainzMash,
		saveMusicBrainzSettings,
		stageBrainzMash as stageBrainzMashMutation,
		testMusicBrainzConnection
	} from '$lib/queries/musicbrainz/MusicBrainzMutations.svelte';
	import { getMusicBrainzSettingsQuery } from '$lib/queries/musicbrainz/MusicBrainzQueries.svelte';
	import type {
		BrainzMashBinding,
		BrainzMashPendingProposal,
		MusicBrainzSettingsResponse,
		MusicBrainzSettingsUpdate,
		MusicBrainzSourceMode
	} from '$lib/queries/musicbrainz/types';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import NumberField from '$lib/ui/NumberField.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import {
		CircleAlert,
		ExternalLink,
		Globe,
		Network,
		Server,
		ShieldCheck,
		Users
	} from 'lucide-svelte';

	import {
		BRAINZMASH_ACTIVE_BINDING_COPY,
		BRAINZMASH_ENDPOINT_URL,
		BRAINZMASH_LOCAL_POLICY_COPY,
		BRAINZMASH_NO_ALTERNATE_PROBE_COPY,
		BRAINZMASH_PENDING_TRANSPORT_COPY,
		BRAINZMASH_PRIVACY_DISCLOSURE,
		BRAINZMASH_RATE_MAX,
		BRAINZMASH_SUPPORTED_ROUTE_FAMILIES,
		BRAINZMASH_TRANSPORT_DISABLED_COPY,
		COMMUNITY_CONFIRM_BUTTON_HINT,
		COMMUNITY_CONFIRM_LABEL,
		COMMUNITY_RISK_BANNER,
		displayVerifyMessage,
		MIRROR_BANNER_LINES,
		MORE_INFO_DISCLOSURES,
		MORE_INFO_SUMMARY,
		MUSICBRAINZ_GUIDE_HREF,
		MUSICBRAINZ_SOURCE_CARDS,
		OFFICIAL_ENDPOINT_URL,
		CLAMPED_WARNING,
		sourceBounds,
		UNLIMITED_RATE_LABEL
	} from './musicBrainzSourceCopy';

	interface MusicBrainzTestResult {
		valid: boolean;
		message: string;
	}

	interface SourceForm {
		source_mode: MusicBrainzSourceMode;
		api_url: string;
		rate_limit: number;
		community_acknowledged: boolean;
	}

	const settingsQuery = getMusicBrainzSettingsQuery();
	const saveMutation = saveMusicBrainzSettings();
	const consentMutation = consentBrainzMash();
	const verifyMutation = testMusicBrainzConnection();
	const stageMutation = stageBrainzMashMutation();
	const activateMutation = activateBrainzMash();

	const CARD_ICONS = {
		brainzmash: Network,
		official: ShieldCheck,
		mirror: Server,
		community: Users
	};

	// The server's latest answer: the query, or a save or lifecycle action's reply.
	let settings = $state.raw<MusicBrainzSettingsResponse | null>(null);
	let testResult = $state<MusicBrainzTestResult | null>(null);
	let actionError = $state('');
	let actionNotice = $state('');
	let providerSweepPending = $state(false);

	$effect.pre(() => {
		const data = settingsQuery.data;
		if (data) settings = data;
	});

	const section = createSettingsSection<SourceForm>(
		{
			get data() {
				return settings ? toForm(settings) : undefined;
			},
			get isPending() {
				return settingsQuery.isPending;
			},
			get isError() {
				return settingsQuery.isError;
			},
			get error() {
				return settingsQuery.error;
			},
			refetch: () => settingsQuery.refetch()
		},
		{
			mutateAsync: async (form) => {
				actionError = '';
				actionNotice = '';
				const previous = settings;
				const next = await saveMutation.mutateAsync(toUpdate(form));
				await syncResponse(previous, next);
				return toForm(next);
			},
			get isPending() {
				return saveMutation.isPending;
			}
		},
		{ savedMessage: 'MusicBrainz settings saved.' }
	);

	let selectedMode = $derived(section.draft?.source_mode ?? 'brainzmash');
	let apiUrl = $derived(section.draft?.api_url ?? '');
	let activeMode = $derived(settings?.source_mode ?? 'brainzmash');
	let pendingBrainzMash = $derived(settings?.pending_brainzmash ?? null);
	let activeBrainzMash = $derived(hasValidActiveBrainzMashBinding(settings));
	let switchingAwayFromBrainz = $derived(activeBrainzMash && selectedMode !== 'brainzmash');
	let activeSourceLabel = $derived(
		MUSICBRAINZ_SOURCE_CARDS.find((card) => card.mode === activeMode)?.title ?? activeMode
	);
	let selectedCard = $derived(
		MUSICBRAINZ_SOURCE_CARDS.find((card) => card.mode === selectedMode) ??
			MUSICBRAINZ_SOURCE_CARDS[0]
	);
	let selectedBounds = $derived(sourceBounds(selectedMode));
	let isBrainzMash = $derived(selectedMode === 'brainzmash');
	let hasConsent = $derived(pendingBrainzMash?.consented === true);
	let hasVerification = $derived(pendingBrainzMash?.verified === true);
	let actionBusy = $derived(
		saveMutation.isPending ||
			consentMutation.isPending ||
			verifyMutation.isPending ||
			stageMutation.isPending ||
			activateMutation.isPending
	);
	let nonBrainzTestPassed = $derived(testResult?.valid === true);
	let nonBrainzTestRequired = $derived(!switchingAwayFromBrainz);
	let communityNeedsAcknowledgment = $derived(
		selectedMode === 'community' && !section.draft?.community_acknowledged
	);
	let saveDisabled = $derived(
		actionBusy ||
			(nonBrainzTestRequired && !nonBrainzTestPassed) ||
			communityNeedsAcknowledgment ||
			!apiUrl.trim()
	);
	// Why Save is off, when the reason is not already on screen.
	let saveHint = $derived(
		switchingAwayFromBrainz
			? ''
			: communityNeedsAcknowledgment
				? COMMUNITY_CONFIRM_BUTTON_HINT
				: nonBrainzTestRequired && !nonBrainzTestPassed
					? 'Test the connection before saving.'
					: ''
	);
	// A quarantined source, or a chosen source that is not yet the active one,
	// can be saved as it stands.
	let needsSave = $derived(
		settings !== null &&
			(settings.source_quarantined === true ||
				(settings.selected_source_mode ?? settings.source_mode) !== settings.source_mode)
	);
	let selectedModeNeedsStage = $derived(
		isBrainzMash && pendingBrainzMash === null && activeMode !== 'brainzmash'
	);
	let displayedTestMessage = $derived(
		testResult ? displayVerifyMessage(testResult.message, selectedMode) : ''
	);

	function hasValidActiveBrainzMashBinding(data: MusicBrainzSettingsResponse | null): boolean {
		if (
			!data ||
			data.source_quarantined === true ||
			data.source_mode !== 'brainzmash' ||
			typeof data.api_url !== 'string' ||
			data.api_url.trim().replace(/\/+$/, '') !== BRAINZMASH_ENDPOINT_URL ||
			typeof data.source_id !== 'string' ||
			!data.source_id.trim() ||
			!Number.isInteger(data.generation) ||
			data.generation < 1
		) {
			return false;
		}
		// The pinned built-in source is active without an interactive disclosure
		// binding. Binding metadata remains optional audit information.
		return true;
	}

	function toForm(data: MusicBrainzSettingsResponse): SourceForm {
		const mode = data.selected_source_mode ?? data.source_mode;
		const fixedWirePolicy = mode === 'brainzmash' || mode === 'official';
		const customModeFromFixedSource =
			(data.source_mode === 'brainzmash' || data.source_mode === 'official') &&
			(mode === 'mirror' || mode === 'community');
		return {
			source_mode: mode,
			api_url: data.source_quarantined
				? mode === 'official'
					? OFFICIAL_ENDPOINT_URL
					: mode === 'brainzmash'
						? BRAINZMASH_ENDPOINT_URL
						: ''
				: (data.api_url ?? ''),
			rate_limit:
				mode === 'brainzmash'
					? BRAINZMASH_RATE_MAX
					: fixedWirePolicy || customModeFromFixedSource
						? 1
						: data.rate_limit,
			community_acknowledged: data.community_acknowledged ?? false
		};
	}

	function toUpdate(form: SourceForm): MusicBrainzSettingsUpdate {
		const mode = form.source_mode;
		const fixedWirePolicy = mode === 'brainzmash' || mode === 'official';
		const rate =
			mode === 'brainzmash' ? BRAINZMASH_RATE_MAX : fixedWirePolicy ? 1 : Number(form.rate_limit);
		return {
			source_mode: mode,
			api_url: mode === 'brainzmash' ? null : form.api_url.trim(),
			rate_limit: Number.isFinite(rate) ? rate : 1,
			community_acknowledged: mode === 'community' ? form.community_acknowledged : null
		};
	}

	function sourceIdentity(data: MusicBrainzSettingsResponse): string {
		return `${data.source_mode}\u0000${data.source_id}\u0000${data.generation}`;
	}

	function bindingFor(proposal: BrainzMashPendingProposal): BrainzMashBinding {
		return {
			access_revision: proposal.access_revision,
			source_id: proposal.source_id,
			generation: proposal.generation,
			disclosure_version: proposal.disclosure_version
		};
	}

	function errorMessage(error: unknown, fallback: string): string {
		return error instanceof Error && error.message.trim() ? error.message : fallback;
	}

	async function sweepProviderCaches(): Promise<void> {
		let succeeded = true;
		try {
			await invalidateMusicBrainzProviderQueries();
		} catch {
			succeeded = false;
		}
		providerSweepPending = !succeeded;
		if (!succeeded) {
			actionError =
				'Settings saved, but cached MusicBrainz data could not be cleared. Save again to retry.';
		}
	}

	async function syncResponse(
		previous: MusicBrainzSettingsResponse | null,
		next: MusicBrainzSettingsResponse
	): Promise<void> {
		const sourceChanged = previous !== null && sourceIdentity(previous) !== sourceIdentity(next);
		settings = next;
		testResult = null;
		if (sourceChanged || providerSweepPending) await sweepProviderCaches();
	}

	// A lifecycle action answers with the new settings; the form follows them.
	async function adoptResponse(
		previous: MusicBrainzSettingsResponse | null,
		next: MusicBrainzSettingsResponse
	): Promise<void> {
		await syncResponse(previous, next);
		await tick();
		section.discard();
	}

	async function stageBrainzMash(): Promise<void> {
		actionError = '';
		actionNotice = '';
		testResult = null;
		const previous = settings;
		try {
			const next = await stageMutation.mutateAsync();
			await adoptResponse(previous, next);
			actionNotice = 'BrainzMash is active. No provider request was made.';
		} catch (error) {
			actionError = errorMessage(error, 'Could not stage BrainzMash.');
		}
	}

	async function acceptBrainzMashDisclosure(): Promise<void> {
		const proposal = pendingBrainzMash;
		if (!proposal || proposal.consented) return;
		actionError = '';
		actionNotice = '';
		try {
			const next = await consentMutation.mutateAsync(bindingFor(proposal));
			await adoptResponse(settings, next);
			actionNotice = 'Disclosure accepted locally. Test Connection is now enabled.';
		} catch (error) {
			actionError = errorMessage(error, 'Could not record BrainzMash consent.');
		}
	}

	async function testConnection(): Promise<void> {
		const proposal = pendingBrainzMash;
		if (switchingAwayFromBrainz) {
			actionError = BRAINZMASH_NO_ALTERNATE_PROBE_COPY;
			return;
		}
		if (isBrainzMash && (!proposal || !proposal.consented)) return;
		actionError = '';
		actionNotice = '';
		testResult = null;
		try {
			if (isBrainzMash) {
				const next = await verifyMutation.mutateAsync(
					bindingFor(proposal as BrainzMashPendingProposal)
				);
				await adoptResponse(settings, next);
			} else if (section.draft) {
				await verifyMutation.mutateAsync(toUpdate(section.draft));
			}
			testResult = {
				valid: true,
				message: isBrainzMash ? 'Connected to BrainzMash.' : 'Connected to MusicBrainz.'
			};
			actionNotice = isBrainzMash
				? 'BrainzMash connection verified. Activate to finish binding this proposal.'
				: 'MusicBrainz connection verified.';
		} catch (error) {
			testResult = { valid: false, message: errorMessage(error, 'Connection test failed.') };
			actionError = testResult.message;
		}
	}

	async function activateBrainzMashSource(): Promise<void> {
		const proposal = pendingBrainzMash;
		if (!proposal || !proposal.consented || !proposal.verified) return;
		actionError = '';
		actionNotice = '';
		try {
			const next = await activateMutation.mutateAsync(bindingFor(proposal));
			await adoptResponse(settings, next);
			actionNotice = 'BrainzMash is active.';
		} catch (error) {
			actionError = errorMessage(error, 'Could not activate BrainzMash.');
		}
	}

	function resetToDefaults(): void {
		void stageBrainzMash();
	}

	function selectMode(form: SourceForm, mode: MusicBrainzSourceMode): void {
		const previousMode = form.source_mode;
		form.source_mode = mode;
		testResult = null;
		actionError = '';
		actionNotice = '';
		if (mode === 'brainzmash') {
			form.api_url = pendingBrainzMash?.endpoint ?? BRAINZMASH_ENDPOINT_URL;
			form.rate_limit = BRAINZMASH_RATE_MAX;
			return;
		}
		if (mode === 'official') {
			form.api_url = OFFICIAL_ENDPOINT_URL;
			form.rate_limit = 1;
		}
		if (mode === 'mirror' || mode === 'community') {
			const normalizedApiUrl = form.api_url.trim().replace(/\/+$/, '');
			const leavingFixedSource = previousMode === 'brainzmash' || previousMode === 'official';
			const usingBuiltInEndpoint =
				normalizedApiUrl === BRAINZMASH_ENDPOINT_URL || normalizedApiUrl === OFFICIAL_ENDPOINT_URL;
			if (usingBuiltInEndpoint) {
				form.api_url = '';
			}
			if (leavingFixedSource || usingBuiltInEndpoint) {
				form.rate_limit = 1;
			}
		}
	}

	function onRadioKeydown(
		event: KeyboardEvent,
		form: SourceForm,
		mode: MusicBrainzSourceMode
	): void {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			selectMode(form, mode);
			return;
		}
		if (
			event.key !== 'ArrowRight' &&
			event.key !== 'ArrowDown' &&
			event.key !== 'ArrowLeft' &&
			event.key !== 'ArrowUp'
		) {
			return;
		}
		event.preventDefault();
		const index = MUSICBRAINZ_SOURCE_CARDS.findIndex((card) => card.mode === mode);
		const delta = event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1 : -1;
		const nextIndex =
			(index + delta + MUSICBRAINZ_SOURCE_CARDS.length) % MUSICBRAINZ_SOURCE_CARDS.length;
		const nextMode = MUSICBRAINZ_SOURCE_CARDS[nextIndex].mode;
		selectMode(form, nextMode);
		const nextRadio = document.getElementById(`musicbrainz-source-${nextMode}`);
		if (nextRadio instanceof HTMLInputElement) {
			nextRadio.focus();
			nextRadio.click();
		}
	}
</script>

<Card title="MusicBrainz">
	<p class="mb-4 text-base-content/70">
		Choose where MusicBrainz data comes from. BrainzMash is the built-in default and is active
		unless you choose a custom mirror or community server.
	</p>

	<QueryBoundary query={section} what="the MusicBrainz settings">
		{#snippet children(draft)}
			<div class="space-y-4">
				{#if settings?.clamped_to_official_limits}
					<div class="alert alert-warning text-sm" role="status">
						<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
						<span>{CLAMPED_WARNING}</span>
					</div>
				{/if}

				{#if settings?.source_quarantined}
					<div class="alert alert-error text-sm" role="alert" data-testid="musicbrainz-quarantined">
						<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
						<span>
							{settings?.quarantine_reason ||
								'Existing MusicBrainz source settings require review. Select and save a valid source to re-enable traffic.'}
						</span>
					</div>
				{/if}

				{#if activeMode !== selectedMode}
					<div class="alert alert-info text-sm" role="status" data-testid="active-source-preserved">
						<Globe class="h-5 w-5 shrink-0" aria-hidden="true" />
						<span>
							Active source: <strong>{activeSourceLabel}</strong>. It stays active until the pending
							source is saved or activated.
						</span>
					</div>
				{/if}
				{#if activeBrainzMash && pendingBrainzMash}
					<div
						class="alert alert-success text-sm"
						role="status"
						data-testid="active-brainzmash-binding"
					>
						<Network class="h-5 w-5 shrink-0" aria-hidden="true" />
						<span>{BRAINZMASH_ACTIVE_BINDING_COPY}</span>
					</div>
				{/if}
				{#if activeBrainzMash && pendingBrainzMash && !isBrainzMash}
					<div
						class="alert alert-info text-sm"
						role="status"
						data-testid="pending-brainzmash-disabled"
					>
						<Network class="h-5 w-5 shrink-0" aria-hidden="true" />
						<span>{BRAINZMASH_PENDING_TRANSPORT_COPY}</span>
					</div>
				{/if}

				<fieldset>
					<legend class="mb-2 text-sm font-semibold">MusicBrainz data source</legend>
					<div class="grid grid-cols-1 gap-3 md:grid-cols-2" data-testid="musicbrainz-source-grid">
						{#each MUSICBRAINZ_SOURCE_CARDS as card (card.mode)}
							{@const CardIcon = CARD_ICONS[card.mode]}
							{@const isSelected = selectedMode === card.mode}
							<label
								class="flex cursor-pointer items-start gap-3 rounded-box border p-4 transition-colors focus-within:outline-2 focus-within:outline-primary {isSelected
									? 'border-primary bg-primary/5'
									: 'border-base-content/15 bg-base-100/60 hover:border-base-content/30'}"
								for={`musicbrainz-source-${card.mode}`}
								data-testid={`musicbrainz-card-${card.mode}`}
							>
								<input
									id={`musicbrainz-source-${card.mode}`}
									class="radio radio-sm mt-0.5 shrink-0"
									type="radio"
									name="musicbrainz-source"
									value={card.mode}
									tabindex={isSelected ? 0 : -1}
									checked={isSelected}
									aria-label={card.title}
									aria-describedby={`musicbrainz-source-${card.mode}-description`}
									onchange={() => selectMode(draft, card.mode)}
									onkeydown={(event) => onRadioKeydown(event, draft, card.mode)}
								/>
								<span class="min-w-0 space-y-2">
									<span class="flex items-center justify-between gap-2">
										<span class="flex items-center gap-2 font-semibold">
											<CardIcon
												class="h-4 w-4 {isSelected ? 'text-primary' : 'text-base-content/60'}"
												aria-hidden="true"
											/>
											{card.title}
										</span>
										{#if card.badge}
											<span class="badge badge-primary badge-sm">{card.badge}</span>
										{/if}
									</span>
									<span
										id={`musicbrainz-source-${card.mode}-description`}
										class="block text-xs text-base-content/60"
									>
										{card.blurb}
									</span>
								</span>
							</label>
						{/each}
					</div>
				</fieldset>

				<details
					class="rounded-box border border-base-content/10 bg-base-100/40 p-3 text-xs text-base-content/70"
				>
					<summary class="cursor-pointer select-none font-medium text-base-content/70">
						{MORE_INFO_SUMMARY}: {selectedCard.title}
					</summary>
					<div class="mt-3 space-y-2">
						{#each MORE_INFO_DISCLOSURES[selectedMode] as paragraph (paragraph.slice(0, 40))}
							<p>{paragraph}</p>
						{/each}
						{#if selectedMode !== 'official' && selectedMode !== 'brainzmash'}
							<a
								href={MUSICBRAINZ_GUIDE_HREF}
								target="_blank"
								rel="noopener noreferrer"
								class="link link-hover inline-flex items-center gap-1 text-primary"
							>
								<ExternalLink class="h-3 w-3" aria-hidden="true" />
								Setup guide
							</a>
						{/if}
					</div>
				</details>

				{#if isBrainzMash}
					<Card
						variant="bordered"
						padding="md"
						as="section"
						aria-labelledby="brainzmash-pending-heading"
					>
						<div class="space-y-3">
							<div class="flex items-start gap-3">
								<Network class="mt-0.5 h-5 w-5 shrink-0 text-primary" aria-hidden="true" />
								<div class="min-w-0 space-y-1">
									<h3 id="brainzmash-pending-heading" class="font-semibold">BrainzMash setup</h3>
									<p class="text-sm text-base-content/70">
										Built-in endpoint: <code
											>{pendingBrainzMash?.endpoint ?? BRAINZMASH_ENDPOINT_URL}</code
										>
									</p>
									<p class="text-xs text-base-content/60">
										Supported read-only route families: {BRAINZMASH_SUPPORTED_ROUTE_FAMILIES.join(
											', '
										)}.
									</p>
								</div>
							</div>

							{#if selectedModeNeedsStage}
								<p class="text-sm text-base-content/70">
									Stage the built-in endpoint locally to receive a fresh pending access revision and
									source identity.
								</p>
								<Button
									variant="primary"
									size="sm"
									loading={stageMutation.isPending}
									disabled={actionBusy}
									onclick={stageBrainzMash}
								>
									Stage BrainzMash
								</Button>
							{:else if pendingBrainzMash}
								<div class="space-y-3">
									<div
										class="rounded-box border border-warning/30 bg-warning/5 p-3 text-sm"
										role="note"
									>
										<p class="font-medium">
											Privacy disclosure {pendingBrainzMash.disclosure_version}
										</p>
										<p class="mt-1 text-base-content/70">{BRAINZMASH_PRIVACY_DISCLOSURE}</p>
									</div>
									<p class="text-sm text-base-content/70">{BRAINZMASH_LOCAL_POLICY_COPY}</p>
									{#if activeBrainzMash}
										<p
											class="text-sm text-base-content/70"
											data-testid="pending-brainzmash-disabled"
										>
											{BRAINZMASH_PENDING_TRANSPORT_COPY}
										</p>
									{:else}
										<p class="text-sm text-base-content/70">{BRAINZMASH_TRANSPORT_DISABLED_COPY}</p>
									{/if}

									<label
										class="flex items-start gap-3 rounded-box border border-base-content/15 bg-base-100/50 p-3"
										for="brainzmash-consent"
									>
										<input
											id="brainzmash-consent"
											class="checkbox checkbox-sm mt-0.5 shrink-0"
											type="checkbox"
											checked={hasConsent}
											disabled={hasConsent || actionBusy}
											aria-label="Accept BrainzMash privacy disclosure"
											onchange={(event) => {
												if ((event.currentTarget as HTMLInputElement).checked)
													void acceptBrainzMashDisclosure();
											}}
										/>
										<span class="text-sm"
											>{COMMUNITY_CONFIRM_LABEL.replace(
												'routing identity data through a server I do not control',
												'routing MusicBrainz query data through BrainzMash'
											)}</span
										>
									</label>

									<div class="flex flex-wrap gap-2">
										<Button
											variant="ghost"
											size="sm"
											loading={verifyMutation.isPending}
											disabled={actionBusy || !hasConsent}
											onclick={testConnection}
										>
											Test Connection
										</Button>
										<Button
											variant="primary"
											size="sm"
											loading={activateMutation.isPending}
											disabled={actionBusy || !hasConsent || !hasVerification}
											onclick={activateBrainzMashSource}
										>
											Activate BrainzMash
										</Button>
									</div>
								</div>
							{:else if activeMode === 'brainzmash'}
								<p class="text-sm text-success">
									BrainzMash is the active runtime source. Traffic uses the local 10 requests/second
									policy.
								</p>
							{/if}
							<Button
								variant="danger-ghost"
								size="sm"
								onclick={resetToDefaults}
								disabled={actionBusy}
							>
								Reset to Defaults
							</Button>
						</div>
					</Card>
				{:else}
					<div class="grid grid-cols-1 gap-x-6 gap-y-4 sm:grid-cols-2">
						{#if selectedMode !== 'official'}
							<div>
								<TextField
									label={selectedMode === 'mirror'
										? 'Mirror API Endpoint URL'
										: 'Community Server API Endpoint URL'}
									placeholder={selectedMode === 'mirror'
										? 'http://mirror-host:5000/ws/2'
										: 'https://mb.example.org/ws/2'}
									help="The full URL to the server's MusicBrainz API, including the version path."
									bind:value={draft.api_url}
								/>
								{#if selectedMode === 'mirror'}
									<p class="mt-1 ml-1 text-xs text-base-content/50">
										Test Connection checks the server is alive. Data vintage is documented in the
										<a
											href={MUSICBRAINZ_GUIDE_HREF}
											target="_blank"
											rel="noopener noreferrer"
											class="link link-hover text-primary">setup guide</a
										>.
									</p>
								{/if}
							</div>
						{:else}
							<Card variant="bordered" padding="md">
								<div>
									<p class="font-medium">Official endpoint</p>
									<p class="mt-1 text-base-content/70">{OFFICIAL_ENDPOINT_URL}</p>
									<p class="mt-1 text-xs text-base-content/50">
										Endpoint editing is disabled for the official source.
									</p>
								</div>
							</Card>
						{/if}

						<div class="space-y-4">
							<NumberField
								label="Rate Limit (requests/sec)"
								min={selectedBounds.allowUnlimitedRate ? 0 : 0.1}
								max={selectedBounds.rateMax}
								step={0.1}
								help={selectedBounds.allowUnlimitedRate
									? `Up to ${selectedBounds.rateMax} req/sec on your own or a chosen server. 0 = ${UNLIMITED_RATE_LABEL}. Be polite with servers you do not own.`
									: 'DroppedNeedle keeps this source at 1 request/second.'}
								bind:value={() => draft.rate_limit, (next) => (draft.rate_limit = Number(next))}
							/>
						</div>
					</div>

					{#if selectedMode === 'community'}
						<div class="alert alert-warning text-sm" role="alert">
							<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
							<span>{COMMUNITY_RISK_BANNER}</span>
						</div>
						<label
							class="label cursor-pointer justify-start gap-3 rounded-box border border-base-content/15 bg-base-100/60 p-4"
							for="community-acknowledgment"
						>
							<input
								id="community-acknowledgment"
								type="checkbox"
								class="checkbox checkbox-sm"
								checked={draft.community_acknowledged}
								onchange={(event) =>
									(draft.community_acknowledged = (
										event.currentTarget as HTMLInputElement
									).checked)}
							/>
							<span class="text-sm">{COMMUNITY_CONFIRM_LABEL}</span>
						</label>
					{/if}

					{#if selectedMode !== 'official'}
						<div class="alert alert-info text-sm" role="status">
							<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
							<div class="space-y-1">
								{#each MIRROR_BANNER_LINES as line (line.slice(0, 40))}<p>{line}</p>{/each}
								<a
									href={MUSICBRAINZ_GUIDE_HREF}
									target="_blank"
									rel="noopener noreferrer"
									class="link link-hover inline-flex items-center gap-1 text-primary"
									><ExternalLink class="h-3 w-3" aria-hidden="true" />Mirror setup guide</a
								>
							</div>
						</div>
					{/if}

					{#if testResult}
						<div
							class="alert"
							class:alert-success={testResult.valid}
							class:alert-error={!testResult.valid}
							role="status"
						>
							<span>{displayedTestMessage}</span>
						</div>
					{/if}

					{#if switchingAwayFromBrainz}
						<div
							class="alert alert-info text-sm"
							role="status"
							data-testid="brainzmash-no-alternate-test"
						>
							<Globe class="h-5 w-5 shrink-0" aria-hidden="true" />
							<span>{BRAINZMASH_NO_ALTERNATE_PROBE_COPY}</span>
						</div>
					{/if}
					<div class="space-y-2 border-t border-base-300 pt-4">
						<div class="flex flex-wrap items-center gap-3">
							<Button
								variant="danger-ghost"
								size="sm"
								onclick={resetToDefaults}
								disabled={actionBusy}
							>
								Reset to Defaults
							</Button>
							<Button
								size="sm"
								loading={verifyMutation.isPending}
								disabled={actionBusy || !apiUrl.trim() || switchingAwayFromBrainz}
								onclick={testConnection}
							>
								Test Connection
							</Button>
							<div class="min-w-0 flex-1">
								<SaveBar
									{...section.bar}
									dirty={section.dirty || needsSave}
									ondiscard={section.dirty ? section.discard : undefined}
									disabled={saveDisabled}
									saveLabel="Save Settings"
								/>
							</div>
						</div>
						{#if saveHint && (section.dirty || needsSave)}
							<p class="text-xs text-base-content/60">{saveHint}</p>
						{/if}
					</div>
				{/if}

				{#if testResult && isBrainzMash}
					<div
						class="alert"
						class:alert-success={testResult.valid}
						class:alert-error={!testResult.valid}
						role="status"
					>
						<span>{displayedTestMessage}</span>
					</div>
				{/if}
				{#if actionError}
					<div class="alert alert-error" role="alert">
						<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" /><span>{actionError}</span>
					</div>
				{/if}
				{#if actionNotice}
					<div class="alert alert-success" role="status" aria-live="polite">
						<span>{actionNotice}</span>
					</div>
				{/if}
			</div>
		{/snippet}
	</QueryBoundary>
</Card>
