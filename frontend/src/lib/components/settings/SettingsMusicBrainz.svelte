<script lang="ts">
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
	import { clearMusicBrainzProviderCaches } from '$lib/utils/albumDetailCache';
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

	let settings = $state<MusicBrainzSettingsResponse | null>(null);
	let selectedMode = $state<MusicBrainzSourceMode>('brainzmash');
	let apiUrl = $state('');
	let rateLimit = $state(1);
	let concurrentSearches = $state(1);
	let communityAcknowledged = $state(false);
	let testResult = $state<MusicBrainzTestResult | null>(null);
	let actionError = $state('');
	let actionNotice = $state('');
	let seeded = $state(false);
	let providerSweepPending = $state(false);
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
		selectedMode === 'community' && !communityAcknowledged
	);
	let saveDisabled = $derived(
		actionBusy ||
			(nonBrainzTestRequired && !nonBrainzTestPassed) ||
			communityNeedsAcknowledgment ||
			!apiUrl.trim()
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

	function cloneSettings(data: MusicBrainzSettingsResponse): MusicBrainzSettingsResponse {
		return {
			...data,
			pending_brainzmash: data.pending_brainzmash ? { ...data.pending_brainzmash } : null
		};
	}

	function seedDraft(data: MusicBrainzSettingsResponse): void {
		settings = cloneSettings(data);
		selectedMode = data.selected_source_mode ?? data.source_mode;
		apiUrl = data.source_quarantined
			? selectedMode === 'official'
				? OFFICIAL_ENDPOINT_URL
				: selectedMode === 'brainzmash'
					? BRAINZMASH_ENDPOINT_URL
					: ''
			: (data.api_url ?? '');
		const fixedWirePolicy = selectedMode === 'brainzmash' || selectedMode === 'official';
		const customModeFromFixedSource =
			(data.source_mode === 'brainzmash' || data.source_mode === 'official') &&
			(selectedMode === 'mirror' || selectedMode === 'community');
		rateLimit =
			selectedMode === 'brainzmash'
				? BRAINZMASH_RATE_MAX
				: fixedWirePolicy || customModeFromFixedSource
					? 1
					: data.rate_limit;
		concurrentSearches =
			fixedWirePolicy || customModeFromFixedSource ? 1 : data.concurrent_searches;
		communityAcknowledged = data.community_acknowledged ?? false;
	}

	function applySettings(data: MusicBrainzSettingsResponse): void {
		seedDraft(data);
		testResult = null;
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
		try {
			if (!clearMusicBrainzProviderCaches()) succeeded = false;
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
		applySettings(next);
		if (sourceChanged || providerSweepPending) await sweepProviderCaches();
	}

	function selectedSettings(): MusicBrainzSettingsUpdate {
		const fixedWirePolicy = selectedMode === 'brainzmash' || selectedMode === 'official';
		const rate =
			selectedMode === 'brainzmash' ? BRAINZMASH_RATE_MAX : fixedWirePolicy ? 1 : Number(rateLimit);
		const concurrency = fixedWirePolicy ? 1 : Number(concurrentSearches);
		return {
			source_mode: selectedMode,
			api_url: selectedMode === 'brainzmash' ? null : apiUrl.trim(),
			rate_limit: Number.isFinite(rate) ? rate : 1,
			concurrent_searches: Number.isFinite(concurrency) ? concurrency : 1,
			community_acknowledged: selectedMode === 'community' ? communityAcknowledged : null
		};
	}

	async function stageBrainzMash(): Promise<void> {
		actionError = '';
		actionNotice = '';
		testResult = null;
		const previous = settings;
		try {
			const next = await stageMutation.mutateAsync();
			await syncResponse(previous, next);
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
			await syncResponse(settings, next);
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
				await syncResponse(settings, next);
			} else {
				await verifyMutation.mutateAsync(selectedSettings());
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
			await syncResponse(settings, next);
			actionNotice = 'BrainzMash is active.';
		} catch (error) {
			actionError = errorMessage(error, 'Could not activate BrainzMash.');
		}
	}

	async function saveNonBrainzSettings(): Promise<void> {
		if (isBrainzMash || saveDisabled) return;
		actionError = '';
		actionNotice = '';
		const previous = settings;
		try {
			const next = await saveMutation.mutateAsync(selectedSettings());
			await syncResponse(previous, next);
			actionNotice = 'MusicBrainz settings saved.';
		} catch (error) {
			actionError = errorMessage(error, 'Could not save MusicBrainz settings.');
		}
	}

	function resetToDefaults(): void {
		void stageBrainzMash();
	}

	function selectMode(mode: MusicBrainzSourceMode): void {
		const previousMode = selectedMode;
		selectedMode = mode;
		testResult = null;
		actionError = '';
		actionNotice = '';
		if (mode === 'brainzmash') {
			apiUrl = pendingBrainzMash?.endpoint ?? BRAINZMASH_ENDPOINT_URL;
			rateLimit = BRAINZMASH_RATE_MAX;
			concurrentSearches = 1;
			return;
		}
		if (mode === 'official') {
			apiUrl = OFFICIAL_ENDPOINT_URL;
			rateLimit = 1;
			concurrentSearches = 1;
		}
		if (mode === 'mirror' || mode === 'community') {
			const normalizedApiUrl = apiUrl.trim().replace(/\/+$/, '');
			const leavingFixedSource = previousMode === 'brainzmash' || previousMode === 'official';
			const usingBuiltInEndpoint =
				normalizedApiUrl === BRAINZMASH_ENDPOINT_URL || normalizedApiUrl === OFFICIAL_ENDPOINT_URL;
			if (usingBuiltInEndpoint) {
				apiUrl = '';
			}
			if (leavingFixedSource || usingBuiltInEndpoint) {
				rateLimit = 1;
				concurrentSearches = 1;
			}
		}
	}

	function onRadioKeydown(event: KeyboardEvent, mode: MusicBrainzSourceMode): void {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			selectMode(mode);
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
		selectMode(nextMode);
		const nextRadio = document.getElementById(`musicbrainz-source-${nextMode}`);
		if (nextRadio instanceof HTMLInputElement) {
			nextRadio.focus();
			nextRadio.click();
		}
	}

	$effect(() => {
		const data = settingsQuery.data;
		if (data && !seeded) {
			seedDraft(data);
			seeded = true;
		}
	});
</script>

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl">MusicBrainz</h2>
		<p class="mb-4 text-base-content/70">
			Choose where MusicBrainz data comes from. BrainzMash is the built-in default and is active
			unless you choose a custom mirror or community server.
		</p>

		{#if settingsQuery.isLoading}
			<div class="flex items-center justify-center py-12">
				<span class="loading loading-spinner loading-lg" aria-label="Loading MusicBrainz settings"
				></span>
			</div>
		{:else if settingsQuery.isError}
			<div class="alert alert-error" role="alert">
				<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
				<span>{errorMessage(settingsQuery.error, 'Could not load MusicBrainz settings.')}</span>
			</div>
		{:else if settings}
			<div class="space-y-4">
				{#if settings.clamped_to_official_limits}
					<div class="alert alert-warning text-sm" role="status">
						<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
						<span>{CLAMPED_WARNING}</span>
					</div>
				{/if}

				{#if settings.source_quarantined}
					<div class="alert alert-error text-sm" role="alert" data-testid="musicbrainz-quarantined">
						<CircleAlert class="h-5 w-5 shrink-0" aria-hidden="true" />
						<span>
							{settings.quarantine_reason ||
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
									class="radio radio-primary mt-0.5 shrink-0"
									type="radio"
									name="musicbrainz-source"
									value={card.mode}
									tabindex={isSelected ? 0 : -1}
									bind:group={selectedMode}
									aria-label={card.title}
									aria-describedby={`musicbrainz-source-${card.mode}-description`}
									onchange={() => selectMode(card.mode)}
									onkeydown={(event) => onRadioKeydown(event, card.mode)}
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
					<section
						class="space-y-3 rounded-box border border-primary/25 bg-primary/5 p-4"
						aria-labelledby="brainzmash-pending-heading"
					>
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
							<button
								class="btn btn-primary btn-sm"
								type="button"
								onclick={stageBrainzMash}
								disabled={actionBusy}
							>
								{#if stageMutation.isPending}<span class="loading loading-spinner loading-xs"
									></span>{/if}
								Stage BrainzMash
							</button>
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
									<p class="text-sm text-base-content/70" data-testid="pending-brainzmash-disabled">
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
										class="checkbox checkbox-primary mt-0.5 shrink-0"
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
									<button
										class="btn btn-ghost btn-sm"
										type="button"
										onclick={testConnection}
										disabled={actionBusy || !hasConsent}
									>
										{#if verifyMutation.isPending}<span class="loading loading-spinner loading-xs"
											></span>{/if}
										Test Connection
									</button>
									<button
										class="btn btn-primary btn-sm"
										type="button"
										onclick={activateBrainzMashSource}
										disabled={actionBusy || !hasConsent || !hasVerification}
									>
										{#if activateMutation.isPending}<span class="loading loading-spinner loading-xs"
											></span>{/if}
										Activate BrainzMash
									</button>
								</div>
							</div>
						{:else if activeMode === 'brainzmash'}
							<p class="text-sm text-success">
								BrainzMash is the active runtime source. Traffic uses the local 10 requests/second
								policy and one concurrent search.
							</p>
						{/if}
						<button
							type="button"
							class="btn btn-outline btn-error btn-sm"
							onclick={resetToDefaults}
							disabled={actionBusy}
						>
							Reset to Defaults
						</button>
					</section>
				{:else}
					<div class="grid grid-cols-1 gap-x-6 gap-y-4 sm:grid-cols-2">
						{#if selectedMode !== 'official'}
							<div class="form-control w-full">
								<label class="label" for="mb-api-url">
									<span class="label-text"
										>{selectedMode === 'mirror'
											? 'Mirror API Endpoint URL'
											: 'Community Server API Endpoint URL'}</span
									>
								</label>
								<input
									id="mb-api-url"
									type="text"
									value={apiUrl}
									class="input w-full"
									placeholder={selectedMode === 'mirror'
										? 'http://mirror-host:5000/ws/2'
										: 'https://mb.example.org/ws/2'}
									oninput={(event) => (apiUrl = (event.currentTarget as HTMLInputElement).value)}
								/>
								<p class="mt-1 ml-1 text-xs text-base-content/50">
									The full URL to the server's MusicBrainz API, including the version path.
								</p>
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
							<div class="rounded-box border border-base-content/10 bg-base-100/40 p-4 text-sm">
								<p class="font-medium">Official endpoint</p>
								<p class="mt-1 text-base-content/70">{OFFICIAL_ENDPOINT_URL}</p>
								<p class="mt-1 text-xs text-base-content/50">
									Endpoint editing is disabled for the official source.
								</p>
							</div>
						{/if}

						<div class="space-y-4">
							<div class="form-control w-full">
								<label class="label" for="mb-rate-limit">
									<span class="label-text">Rate Limit (requests/sec)</span>
									{#if selectedBounds.allowUnlimitedRate}<span class="badge badge-info badge-sm"
											>{UNLIMITED_RATE_LABEL}</span
										>{/if}
								</label>
								<input
									id="mb-rate-limit"
									type="number"
									min={selectedBounds.allowUnlimitedRate ? 0 : 0.1}
									max={selectedBounds.rateMax}
									step="0.1"
									value={rateLimit}
									class="input w-full"
									oninput={(event) =>
										(rateLimit = Number((event.currentTarget as HTMLInputElement).value))}
								/>
								<p class="mt-1 ml-1 text-xs text-base-content/50">
									{#if selectedBounds.allowUnlimitedRate}Up to {selectedBounds.rateMax} req/sec on your
										own or a chosen server. 0 = {UNLIMITED_RATE_LABEL}. Be polite with servers you
										do not own.{:else}DroppedNeedle keeps this source at 1 request/second.{/if}
								</p>
							</div>
							<div class="form-control w-full">
								<label class="label" for="mb-concurrent"
									><span class="label-text">Concurrent Searches</span></label
								>
								<input
									id="mb-concurrent"
									type="number"
									min="1"
									max={selectedBounds.concurrentMax}
									value={concurrentSearches}
									class="input w-full"
									oninput={(event) =>
										(concurrentSearches = Number((event.currentTarget as HTMLInputElement).value))}
								/>
								<p class="mt-1 ml-1 text-xs text-base-content/50">
									Local queue capacity, maximum {selectedBounds.concurrentMax}.
								</p>
							</div>
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
								class="checkbox checkbox-primary"
								checked={communityAcknowledged}
								onchange={(event) =>
									(communityAcknowledged = (event.currentTarget as HTMLInputElement).checked)}
							/>
							<span class="label-text text-sm">{COMMUNITY_CONFIRM_LABEL}</span>
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
					<div class="flex flex-wrap items-center justify-between gap-3 pt-2">
						<button
							type="button"
							class="btn btn-outline btn-error btn-sm"
							onclick={resetToDefaults}
							disabled={actionBusy}>Reset to Defaults</button
						>
						<div class="flex gap-2">
							<button
								type="button"
								class="btn btn-ghost"
								onclick={testConnection}
								disabled={actionBusy || !apiUrl.trim() || switchingAwayFromBrainz}
							>
								{#if verifyMutation.isPending}<span class="loading loading-spinner loading-sm"
									></span>{/if}Test Connection
							</button>
							<div
								class="tooltip"
								data-tip={switchingAwayFromBrainz
									? BRAINZMASH_NO_ALTERNATE_PROBE_COPY
									: communityNeedsAcknowledgment
										? COMMUNITY_CONFIRM_BUTTON_HINT
										: 'Test connection before saving'}
							>
								<button
									type="button"
									class="btn btn-primary"
									onclick={saveNonBrainzSettings}
									disabled={saveDisabled}
								>
									{#if saveMutation.isPending}<span class="loading loading-spinner loading-sm"
										></span>{/if}Save Settings
								</button>
							</div>
						</div>
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
		{/if}
	</div>
</div>
