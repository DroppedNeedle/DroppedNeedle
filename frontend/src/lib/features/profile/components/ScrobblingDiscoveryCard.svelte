<script lang="ts">
	import {
		Radio,
		Music,
		LoaderCircle,
		ExternalLink,
		Check,
		Sparkles,
		Eraser,
		Upload
	} from 'lucide-svelte';
	import { ApiError } from '$lib/api/errors';
	import { getIntegrationStatusQuery } from '$lib/features/home/queries/home.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { getConnectionsQuery } from '$lib/features/profile/queries/ConnectionsQuery.svelte';
	import {
		createConnectListenBrainzMutation,
		createDisconnectMutation,
		createLastFmExchangeSessionMutation,
		createLastFmRequestTokenMutation,
		createSetLastFmCredentialsMutation
	} from '$lib/features/profile/queries/ConnectionsMutations.svelte';
	import { getScrobblePreferencesQuery } from '$lib/features/profile/queries/scrobble-preferences.svelte';
	import {
		createRefreshPersonalMixMutation,
		createUpdateScrobblePreferencesMutation
	} from '$lib/features/profile/queries/scrobble-preferences.svelte';
	import type {
		NowPlayingVisibility,
		ScrobblePreferencesUpdate
	} from '$lib/features/profile/queries/scrobble-preferences.svelte';
	import type { MusicSource } from '$lib/stores/musicSource';
	import { createForgetDiscoverHistoryMutation } from '$lib/features/profile/queries/discover-history.svelte';
	import {
		createImportListeningHistoryMutation,
		createUploadListeningHistoryMutation,
		getListeningHistoryQuery
	} from '$lib/features/profile/queries/listening-history.svelte';
	import type { ListeningHistory } from '$lib/features/profile/queries/listening-history.svelte';
	import { scrobbleManager } from '$lib/features/player/state/scrobble.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	interface Props {
		navidromeEnabled: boolean;
	}

	let { navidromeEnabled }: Props = $props();

	const connectionsQuery = getConnectionsQuery();
	const connections = $derived(connectionsQuery.data?.connections ?? []);
	const lb = $derived(connections.find((c) => c.service === 'listenbrainz'));
	const lfm = $derived(connections.find((c) => c.service === 'lastfm'));
	// A user's own API key pair is stored as the Last.fm connection before the
	// sign-in finishes; the account name arrives only with the session, so a
	// link without one is not linked yet and still needs Connect and Finish.
	const lfmLinked = $derived(Boolean(lfm?.username));
	// The admin's switch. While it is off, Connect could only fail.
	const integrationQuery = getIntegrationStatusQuery();
	const lfmOff = $derived(integrationQuery.data?.lastfm === false);
	const lfmNote = $derived(lfmOff ? 'Off on this server.' : 'Link Last.fm above to scrobble.');

	const prefsQuery = getScrobblePreferencesQuery();
	const prefs = $derived(prefsQuery.data);

	const requestTokenMutation = createLastFmRequestTokenMutation();
	const exchangeSessionMutation = createLastFmExchangeSessionMutation();
	const setLastFmCredentialsMutation = createSetLastFmCredentialsMutation();
	const connectLbMutation = createConnectListenBrainzMutation();
	const disconnectMutation = createDisconnectMutation();
	const updatePrefsMutation = createUpdateScrobblePreferencesMutation();
	const refreshMixMutation = createRefreshPersonalMixMutation();
	const forgetHistoryMutation = createForgetDiscoverHistoryMutation();
	let forgetOpen = $state(false);

	const historyQuery = getListeningHistoryQuery();
	const uploadHistoryMutation = createUploadListeningHistoryMutation();
	let historyInput = $state<HTMLInputElement | null>(null);
	let historyError = $state<string | null>(null);
	const importHistoryMutation = createImportListeningHistoryMutation();
	const fromExport = (history: ListeningHistory) => history.source === 'export';
	function checkedAgo(seconds: number): string {
		const minutes = Math.floor((Date.now() / 1000 - seconds) / 60);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes} ${minutes === 1 ? 'minute' : 'minutes'} ago`;
		const hours = Math.floor(minutes / 60);
		if (hours < 24) return `${hours} ${hours === 1 ? 'hour' : 'hours'} ago`;
		const days = Math.floor(hours / 24);
		return `${days} ${days === 1 ? 'day' : 'days'} ago`;
	}
	function historyLine(history: ListeningHistory): string {
		if (history.state === 'pending' || history.state === 'exporting')
			return 'Importing your ListenBrainz history';
		if (history.state === 'importing')
			return fromExport(history) ? 'Importing your ListenBrainz history' : 'Reading your file';
		if (history.state === 'failed')
			return fromExport(history)
				? "Your ListenBrainz history couldn't be imported."
				: 'Your file could not be read.';
		if (history.state === 'forgotten') return 'Forgotten. Nothing is imported until you ask.';
		const count = history.imported_listens;
		if (count === 0) return 'Nothing imported yet.';
		const listens = `${count.toLocaleString()} ${count === 1 ? 'listen' : 'listens'} from ListenBrainz`;
		return history.account && history.last_synced_at
			? `${listens}, checked ${checkedAgo(history.last_synced_at)}`
			: listens;
	}
	const canImportAgain = (history: ListeningHistory) =>
		Boolean(lb) && (history.state === 'forgotten' || history.state === 'failed');
	async function importAgain() {
		historyError = null;
		try {
			await importHistoryMutation.mutateAsync();
		} catch (e) {
			historyError = errorMessage(e, "Couldn't start the import.");
		}
	}

	// Linking another account than the one the history is kept from asks
	// first: the kept history is replaced.
	let replacing = $state<{ from: string; to: string } | null>(null);
	let replaceOpen = $state(false);

	const loading = $derived(connectionsQuery.isPending || prefsQuery.isPending);

	const SOURCES: { value: MusicSource; label: string }[] = [
		{ value: 'listenbrainz', label: 'ListenBrainz' },
		{ value: 'lastfm', label: 'Last.fm' }
	];

	let lbFormOpen = $state(false);
	let lbToken = $state('');
	let lbUsername = $state('');
	let lbError = $state<string | null>(null);

	let lfmPendingToken = $state<string | null>(null);
	let lfmError = $state<string | null>(null);
	let lfmCredentialsOpen = $state(false);
	let lfmApiKey = $state('');
	let lfmSecret = $state('');

	// optimistic mirror so toggles feel instant; re-synced whenever the query settles
	let scrobbleLastfm = $state(false);
	let scrobbleListenbrainz = $state(false);
	let navidromeHandlesExternalScrobbles = $state(true);
	let primarySource = $state<MusicSource>('listenbrainz');
	// While Last.fm is off, Home and Discover follow ListenBrainz whatever the
	// stored choice, so that is the one shown.
	const shownSource = $derived<MusicSource>(lfmOff ? 'listenbrainz' : primarySource);
	let nowPlayingVisibility = $state<NowPlayingVisibility>('full');
	let autoRequestPersonalMix = $state(false);
	let mixRefreshMessage = $state<string | null>(null);
	const autoRequestState = $derived(prefs?.auto_request_state ?? 'none');
	const autoRequestNote = $derived.by(() => {
		if (!lb) return 'Link ListenBrainz above to enable.';
		if (autoRequestPersonalMix && autoRequestState === 'pending')
			return 'Waiting for an admin to approve auto-requests.';
		if (autoRequestState === 'rejected' || autoRequestState === 'revoked')
			return 'An admin declined auto-requests for your mix.';
		return undefined;
	});
	const visibilityId = $props.id();
	$effect(() => {
		if (prefs) {
			scrobbleLastfm = prefs.scrobble_to_lastfm ?? false;
			scrobbleListenbrainz = prefs.scrobble_to_listenbrainz ?? false;
			navidromeHandlesExternalScrobbles = prefs.navidrome_handles_external_scrobbles ?? true;
			primarySource = prefs.primary_music_source ?? 'listenbrainz';
			nowPlayingVisibility = prefs.now_playing_visibility ?? 'full';
			autoRequestPersonalMix = prefs.auto_request_personal_mix ?? false;
		}
	});

	function errorMessage(e: unknown, fallback: string): string {
		return e instanceof ApiError ? e.message : fallback;
	}

	async function connectListenBrainz(replaceHistory = false) {
		lbError = null;
		try {
			await connectLbMutation.mutateAsync({
				user_token: lbToken.trim(),
				username: lbUsername.trim(),
				replace_history: replaceHistory
			});
			lbFormOpen = false;
			lbToken = '';
			lbUsername = '';
		} catch (e) {
			if (e instanceof ApiError && e.code === 'HISTORY_ACCOUNT_DIFFERS') {
				const kept = (e.details as { kept_account?: unknown } | null)?.kept_account;
				replacing = {
					from: typeof kept === 'string' ? kept : 'your other account',
					to: lbUsername.trim()
				};
				replaceOpen = true;
				return;
			}
			lbError = errorMessage(e, 'Could not verify that token.');
		}
	}

	async function startLastFm() {
		lfmError = null;
		try {
			const data = await requestTokenMutation.mutateAsync();
			lfmPendingToken = data.token;
			window.open(data.auth_url, '_blank', 'popup=yes,noopener,noreferrer');
		} catch (e) {
			// No Last.fm app key to link with: the admin has not saved the server's
			// key pair and this user has not saved their own.
			if (e instanceof ApiError && e.code === 'PROVIDER_NOT_CONFIGURED') {
				lfmCredentialsOpen = true;
				return;
			}
			lfmError = errorMessage(e, 'Could not start Last.fm sign-in.');
		}
	}

	async function saveLastFmCredentials() {
		lfmError = null;
		try {
			await setLastFmCredentialsMutation.mutateAsync({
				api_key: lfmApiKey.trim(),
				shared_secret: lfmSecret.trim()
			});
			lfmCredentialsOpen = false;
			lfmApiKey = '';
			lfmSecret = '';
			await startLastFm();
		} catch (e) {
			lfmError = errorMessage(e, 'Could not save your Last.fm credentials.');
		}
	}

	async function finishLastFm() {
		if (!lfmPendingToken) return;
		lfmError = null;
		try {
			await exchangeSessionMutation.mutateAsync(lfmPendingToken);
			lfmPendingToken = null;
		} catch (e) {
			lfmError = errorMessage(e, "Authorization isn't complete yet - approve it, then retry.");
		}
	}

	function cancelLastFm() {
		lfmPendingToken = null;
		lfmError = null;
	}

	async function disconnect(service: string) {
		await disconnectMutation.mutateAsync(service);
		if (service === 'lastfm') lfmPendingToken = null;
		await scrobbleManager.refreshSettings();
	}

	async function savePrefs(update: ScrobblePreferencesUpdate) {
		try {
			await updatePrefsMutation.mutateAsync(update);
			await scrobbleManager.refreshSettings();
		} catch {
			if (prefs) {
				scrobbleLastfm = prefs.scrobble_to_lastfm ?? false;
				scrobbleListenbrainz = prefs.scrobble_to_listenbrainz ?? false;
				navidromeHandlesExternalScrobbles = prefs.navidrome_handles_external_scrobbles ?? true;
				primarySource = prefs.primary_music_source ?? 'listenbrainz';
				nowPlayingVisibility = prefs.now_playing_visibility ?? 'full';
				autoRequestPersonalMix = prefs.auto_request_personal_mix ?? false;
			}
		}
	}

	async function selectSource(src: MusicSource) {
		if (src === primarySource) return;
		primarySource = src;
		await savePrefs({ primary_music_source: src });
	}

	async function refreshPersonalMix() {
		mixRefreshMessage = null;
		try {
			const result = await refreshMixMutation.mutateAsync();
			// the build runs in the background; completion arrives as a toast via
			// the personal_mix_refreshed SSE event
			mixRefreshMessage =
				result.status === 'already_running'
					? 'A refresh is already running - hang tight.'
					: "Refreshing in the background - we'll let you know when it's ready.";
		} catch (e) {
			mixRefreshMessage = errorMessage(e, "Couldn't refresh Your Weekly Mix.");
		}
	}

	async function uploadHistory(event: Event) {
		const input = event.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		// Cleared, so choosing the same file again uploads it again.
		input.value = '';
		if (!file) return;
		historyError = null;
		try {
			await uploadHistoryMutation.mutateAsync(file);
		} catch (e) {
			historyError =
				e instanceof ApiError && e.status === 413
					? 'That file is over 512 MB.'
					: errorMessage(e, 'Could not upload that file.');
		}
	}

	async function forgetHistory() {
		try {
			await forgetHistoryMutation.mutateAsync();
			historyError = null;
		} catch (e) {
			if (e instanceof ApiError && e.status === 429) {
				throw new Error('Too many tries just now. Try again in a minute.', { cause: e });
			}
			// Each step can run again, so asking again finishes the rest.
			throw new Error('Not all of it was removed. Forget again to finish.', { cause: e });
		}
	}
</script>

{#snippet serviceRow(
	Icon: typeof Music,
	tint: string,
	name: string,
	username: string | null | undefined,
	linked: boolean
)}
	<div class="flex min-w-0 items-center gap-3">
		<span class="sc-icon {tint}" aria-hidden="true">
			<Icon class="size-5" />
		</span>
		<div class="min-w-0">
			<div class="flex items-center gap-2">
				<span class="font-semibold">{name}</span>
				<span
					class="status status-sm {linked ? 'status-success' : 'status-error'}"
					aria-hidden="true"
				></span>
			</div>
			{#if linked}
				<p class="pf-meta truncate">@{username || 'linked'}</p>
			{:else}
				<p class="pf-meta">Not connected</p>
			{/if}
		</div>
	</div>
{/snippet}

<div class="space-y-3">
	{#if loading}
		<div class="flex items-center justify-center py-10">
			<LoaderCircle class="h-5 w-5 animate-spin motion-keep text-muted" />
		</div>
	{:else}
		<div>
			<div class="pf-box px-4 py-3">
				<div class="flex items-center justify-between gap-3">
					{@render serviceRow(
						Music,
						'text-listenbrainz',
						'ListenBrainz',
						lb?.username,
						Boolean(lb)
					)}
					<div class="shrink-0">
						{#if lb}
							<Button
								variant="outline"
								size="display-sm"
								onclick={() => disconnect('listenbrainz')}
								disabled={disconnectMutation.isPending}
							>
								Disconnect
							</Button>
						{:else}
							<Button
								variant="listenbrainz"
								size="display-sm"
								onclick={() => (lbFormOpen = !lbFormOpen)}
							>
								Connect
							</Button>
						{/if}
					</div>
				</div>
			</div>

			{#if !lb && lbFormOpen}
				<div class="pf-box mt-2 animate-fade-in-up p-3">
					<div class="space-y-2">
						<p class="text-xs text-muted">
							Paste your ListenBrainz user token and username to link your account.
						</p>
						<Input
							type="password"
							label="ListenBrainz user token"
							size="sm"
							class="w-full"
							placeholder="User token"
							autocomplete="off"
							bind:value={lbToken}
						/>
						<Input
							label="ListenBrainz username"
							size="sm"
							class="w-full"
							placeholder="ListenBrainz username"
							autocomplete="off"
							bind:value={lbUsername}
						/>
						{#if lbError}
							<p class="text-xs text-error">{lbError}</p>
						{/if}
						<div class="flex justify-end gap-2">
							<Button
								variant="ghost"
								size="sm"
								onclick={() => {
									lbFormOpen = false;
									lbError = null;
								}}
							>
								Cancel
							</Button>
							<Button
								variant="primary"
								size="sm"
								loading={connectLbMutation.isPending}
								disabled={!lbToken.trim() || !lbUsername.trim()}
								onclick={() => connectListenBrainz()}
							>
								Link account
							</Button>
						</div>
					</div>
				</div>
			{/if}

			<div class="pf-box mt-2 px-4 py-3">
				<div class="flex flex-wrap items-center justify-between gap-3">
					<div class="min-w-0">
						<span class="text-sm font-semibold">Listening history</span>
						<QueryBoundary query={historyQuery} what="your listening history">
							{#snippet children(history)}
								<p class="pf-meta">{historyLine(history)}</p>
								{#if canImportAgain(history)}
									<Button
										variant="ghost"
										size="xs"
										loading={importHistoryMutation.isPending}
										onclick={importAgain}
									>
										Import again
									</Button>
								{/if}
							{/snippet}
							{#snippet loading()}
								<p class="pf-meta">Checking your history</p>
							{/snippet}
							{#snippet failed(_error, retry)}
								<p class="pf-meta">
									Couldn't check your history.
									<Button variant="ghost" size="xs" onclick={retry}>Try again</Button>
								</p>
							{/snippet}
						</QueryBoundary>
					</div>
					<input
						type="file"
						accept=".zip,application/zip"
						class="hidden"
						aria-label="Choose a ListenBrainz export file"
						bind:this={historyInput}
						onchange={uploadHistory}
					/>
					<Button
						variant="outline"
						size="sm"
						icon={Upload}
						loading={uploadHistoryMutation.isPending}
						onclick={() => historyInput?.click()}
					>
						Add a ListenBrainz export file
					</Button>
				</div>
				{#if historyError}
					<p class="mt-2 text-xs text-error">{historyError}</p>
				{/if}
			</div>
		</div>

		<div>
			<div class="pf-box px-4 py-3">
				<div class="flex items-center justify-between gap-3">
					{@render serviceRow(Radio, 'text-lastfm', 'Last.fm', lfm?.username, lfmLinked)}
					<div class="flex shrink-0 items-center gap-2">
						{#if lfm}
							<Button
								variant="outline"
								size="display-sm"
								onclick={() => disconnect('lastfm')}
								disabled={disconnectMutation.isPending}
							>
								Disconnect
							</Button>
						{/if}
						{#if !lfmLinked && lfmPendingToken}
							<div class="flex items-center gap-2">
								<Button variant="ghost" size="sm" onclick={cancelLastFm}>Cancel</Button>
								<Button
									variant="primary"
									size="display-sm"
									icon={Check}
									loading={exchangeSessionMutation.isPending}
									onclick={finishLastFm}
								>
									Finish
								</Button>
							</div>
						{:else if !lfmLinked && !lfmOff}
							<Button
								variant="lastfm"
								size="display-sm"
								icon={ExternalLink}
								loading={requestTokenMutation.isPending}
								onclick={startLastFm}
							>
								Connect
							</Button>
						{/if}
					</div>
				</div>
			</div>
			{#if lfmOff && !lfmLinked}
				<p class="mt-2 px-1 text-xs text-muted">
					Last.fm is off on this server.
					{#if authStore.isAdmin}
						<a class="link" href={withBasePath('/settings?tab=lastfm')}>Settings &gt; Last.fm</a>
					{/if}
				</p>
			{/if}
			{#if !lfmLinked && !lfmOff && lfmCredentialsOpen}
				<div class="pf-box mt-2 animate-fade-in-up p-3">
					<div class="space-y-2">
						<p class="text-xs text-muted">
							Enter the API key and shared secret of your own
							<a
								href="https://www.last.fm/api/account/create"
								target="_blank"
								rel="noopener noreferrer"
								class="link">Last.fm API account</a
							>, then connect.
						</p>
						<Input
							label="Last.fm API key"
							size="sm"
							class="w-full"
							placeholder="API key"
							autocomplete="off"
							bind:value={lfmApiKey}
						/>
						<Input
							type="password"
							label="Last.fm shared secret"
							size="sm"
							class="w-full"
							placeholder="Shared secret"
							autocomplete="off"
							bind:value={lfmSecret}
						/>
						<div class="flex justify-end gap-2">
							<Button variant="ghost" size="sm" onclick={() => (lfmCredentialsOpen = false)}>
								Cancel
							</Button>
							<Button
								variant="primary"
								size="sm"
								loading={setLastFmCredentialsMutation.isPending}
								disabled={!lfmApiKey.trim() || !lfmSecret.trim()}
								onclick={saveLastFmCredentials}
							>
								Save and connect
							</Button>
						</div>
					</div>
				</div>
			{/if}
			{#if lfmPendingToken && !lfmLinked}
				<p class="mt-2 px-1 text-xs text-muted animate-fade-in-up">
					Approve DroppedNeedle in the Last.fm window that opened, then choose Finish.
				</p>
			{/if}
			{#if lfmError}
				<p class="mt-2 px-1 text-xs text-error">{lfmError}</p>
			{/if}
		</div>

		<hr class="border-line" />

		<div class="space-y-3 px-1">
			<ToggleRow
				label="Scrobble to Last.fm"
				description={lfmLinked && !lfmOff ? undefined : lfmNote}
				disabled={!lfmLinked || lfmOff || updatePrefsMutation.isPending}
				bind:checked={
					() => scrobbleLastfm,
					(checked) => {
						scrobbleLastfm = checked;
						void savePrefs({ scrobble_to_lastfm: checked });
					}
				}
			/>

			<ToggleRow
				label="Scrobble to ListenBrainz"
				description={lb ? undefined : 'Link ListenBrainz above to scrobble.'}
				disabled={!lb || updatePrefsMutation.isPending}
				bind:checked={
					() => scrobbleListenbrainz,
					(checked) => {
						scrobbleListenbrainz = checked;
						void savePrefs({ scrobble_to_listenbrainz: checked });
					}
				}
			/>

			{#if navidromeEnabled}
				<ToggleRow
					label="Let Navidrome handle Last.fm and ListenBrainz"
					description="Keep this on while Navidrome sends plays to those accounts. To use DroppedNeedle instead, first disable external scrobbles for the DroppedNeedle player in Navidrome. Listening history is recorded either way."
					disabled={updatePrefsMutation.isPending}
					bind:checked={
						() => navidromeHandlesExternalScrobbles,
						(checked) => {
							navidromeHandlesExternalScrobbles = checked;
							void savePrefs({ navidrome_handles_external_scrobbles: checked });
						}
					}
				/>
			{/if}
		</div>

		<hr class="border-line" />

		<div class="flex flex-wrap items-center justify-between gap-3 px-1">
			<div>
				<span class="text-sm font-medium">Primary source</span>
				<p class="text-xs text-muted">Powers your Home &amp; Discover by default.</p>
			</div>
			<div
				class="inline-flex gap-1 rounded-full bg-base-300/50 p-1"
				role="group"
				aria-label="Primary source"
			>
				{#each SOURCES as opt (opt.value)}
					<Button
						variant={shownSource === opt.value ? 'primary' : 'ghost'}
						size="sm"
						aria-pressed={shownSource === opt.value}
						disabled={updatePrefsMutation.isPending || (lfmOff && opt.value === 'lastfm')}
						onclick={() => selectSource(opt.value)}
					>
						{opt.label}
					</Button>
				{/each}
			</div>
		</div>

		<hr class="border-line" />

		<div class="flex flex-wrap items-center justify-between gap-3 px-1">
			<div>
				<label for={visibilityId} class="text-sm font-medium">Now Playing visibility</label>
				<p class="text-xs text-muted">How your listening appears to others on the home page.</p>
			</div>
			<select
				id={visibilityId}
				class="select select-sm w-full max-w-[16rem] text-xs"
				bind:value={nowPlayingVisibility}
				disabled={updatePrefsMutation.isPending}
				onchange={() => savePrefs({ now_playing_visibility: nowPlayingVisibility })}
			>
				<option value="full">Show what I'm playing</option>
				<option value="track_hidden">Show I'm listening, hide the track</option>
				<option value="offline">Don't show my activity</option>
			</select>
		</div>

		<hr class="border-line" />

		<div class="space-y-2 px-1">
			<div class="flex items-center gap-2">
				<Sparkles class="h-4 w-4 text-accent" />
				<span class="text-sm font-semibold">Your Weekly Mix</span>
			</div>
			<p class="text-xs text-muted">
				A playlist built from your ListenBrainz listening history, refreshed weekly.
			</p>
			<ToggleRow
				label="Auto-request missing music from Your Weekly Mix"
				description={autoRequestNote}
				disabled={!lb || updatePrefsMutation.isPending}
				bind:checked={
					() => autoRequestPersonalMix,
					(checked) => {
						autoRequestPersonalMix = checked;
						void savePrefs({ auto_request_personal_mix: checked });
					}
				}
			/>
			<div class="flex items-center gap-3">
				<Button
					variant="outline"
					size="sm"
					loading={refreshMixMutation.isPending}
					disabled={!lb}
					onclick={refreshPersonalMix}
				>
					Refresh my mix
				</Button>
				{#if mixRefreshMessage}
					<p class="grow-0 text-xs text-muted">{mixRefreshMessage}</p>
				{/if}
			</div>
		</div>

		<hr class="border-line" />

		<div class="space-y-2 px-1">
			<span class="text-sm font-semibold">Your history</span>
			<p class="text-xs text-muted">
				Kept for you: your plays here, a copy of your ListenBrainz history, what you told Discover,
				the picks it showed you and how your plays ended. Backups made earlier still hold what they
				held.
			</p>
			<Button
				variant="danger-outline"
				size="display-sm"
				icon={Eraser}
				onclick={() => (forgetOpen = true)}
			>
				Forget my history
			</Button>
		</div>
	{/if}
</div>

<ConfirmDialog
	bind:open={forgetOpen}
	title="Forget your history?"
	message="This removes the picks, the play endings and the copy of your ListenBrainz history. Play counts, your library and what you told Discover stay."
	confirmLabel="Forget"
	tone="danger"
	pending={forgetHistoryMutation.isPending}
	onconfirm={forgetHistory}
/>

<ConfirmDialog
	bind:open={replaceOpen}
	title="Replace your listening history?"
	message={`Replace the history kept from ${replacing?.from ?? ''} with ${replacing?.to ?? ''}'s?`}
	confirmLabel="Replace"
	tone="danger"
	pending={connectLbMutation.isPending}
	onconfirm={() => connectListenBrainz(true)}
	onclose={() => (replacing = null)}
/>

<style>
	.sc-icon {
		display: grid;
		flex: none;
		place-items: center;
		width: 2.75rem;
		height: 2.75rem;
		border-radius: 0.5rem;
		background-color: var(--color-base-300);
	}
</style>
