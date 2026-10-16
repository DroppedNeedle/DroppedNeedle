<script lang="ts">
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import { api } from '$lib/api/client';
	import { PLEX_ENDPOINTS } from '$lib/queries/plex/endpoints';
	import { pollPlexFlow, startPlexFlow } from '$lib/queries/plex/PlexFlowApi';
	import { resetPlexScrobblePreference } from '$lib/player/plexPlaybackApi';
	import type { components } from '$lib/api/v3/openapi';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';

	type PlexConnectionSettings = components['schemas']['PlexConnection'];
	type PlexLibrarySection = components['schemas']['PlexLibrarySectionInfo'];

	type PlexTestResult = components['schemas']['PlexVerifyResponse'];
	type PlexSettingsForm = ReturnType<typeof createSettingsForm<PlexConnectionSettings>> & {
		testResult: PlexTestResult | null;
	};

	const form = createSettingsForm<PlexConnectionSettings>({
		loadEndpoint: PLEX_ENDPOINTS.settings(),
		saveEndpoint: PLEX_ENDPOINTS.settings(),
		testEndpoint: PLEX_ENDPOINTS.verify(),
		enabledField: 'enabled',
		secondaryEnabledField: 'login_enabled',
		refreshIntegration: true,
		afterTest: (result) => {
			const typed = result as PlexTestResult;
			if (typed.valid) {
				libraries = typed.libraries ?? [];
				if (form.data?.music_library_ids) {
					const validKeys = new Set(libraries.map((l) => l.key));
					form.data.music_library_ids = form.data.music_library_ids.filter((id) =>
						validKeys.has(id)
					);
				}
			}
		}
	}) as PlexSettingsForm;

	const TOKEN_MASK = 'plex****';
	let oauthPending = $state(false);
	let oauthUrl = $state<string | null>(null);
	let libraries = $state<PlexLibrarySection[]>([]);
	let loadingLibraries = $state(false);

	async function save() {
		await form.save();
		resetPlexScrobblePreference();
	}

	async function test() {
		await form.test();
	}

	async function startOAuth() {
		oauthPending = true;
		oauthUrl = null;
		try {
			const res = await startPlexFlow('connect');
			oauthUrl = res.authorize_url;
			window.open(res.authorize_url, '_blank', 'noopener');
			await pollForToken(res.pin_id);
		} catch {
			oauthPending = false;
		}
	}

	async function pollForToken(pinId: number) {
		const maxAttempts = 60;
		for (let i = 0; i < maxAttempts; i++) {
			await new Promise((r) => setTimeout(r, 3000));
			if (!oauthPending) return;
			try {
				const res = await pollPlexFlow('connect', pinId);
				if (!oauthPending) return;
				if (res.completed && res.auth_token) {
					if (form.data) form.data.plex_token = res.auth_token;
					oauthPending = false;
					oauthUrl = null;
					await test();
					return;
				}
			} catch {
				break;
			}
		}
		oauthPending = false;
		oauthUrl = null;
	}

	function cancelOAuth() {
		oauthPending = false;
		oauthUrl = null;
	}

	async function fetchLibraries() {
		loadingLibraries = true;
		try {
			libraries = await api.global.v3.GET(PLEX_ENDPOINTS.libraries());
		} catch {
			libraries = [];
		}
		loadingLibraries = false;
	}

	function toggleLibrary(key: string) {
		if (!form.data) return;
		const ids = form.data.music_library_ids ?? [];
		if (ids.includes(key)) {
			form.data.music_library_ids = ids.filter((k) => k !== key);
		} else {
			form.data.music_library_ids = [...ids, key];
		}
	}

	let hasCredentials = $derived(Boolean(form.data?.plex_url && form.data?.plex_token));
	let hasLibrarySelected = $derived(Boolean(form.data?.music_library_ids?.length));

	$effect(() => {
		(async () => {
			await form.load();
			if (form.data?.plex_url && form.data?.plex_token) {
				await fetchLibraries();
			}
		})();
	});

	onDestroy(() => {
		form.cleanup();
		oauthPending = false;
	});
</script>

<Card title="Plex Connection">
	<p class="text-base-content/70 mb-4">
		Connect Plex to browse your library, play music, and keep your listening history in sync.
	</p>

	{#if form.loading}
		<div class="flex justify-center items-center py-12">
			<span class="loading loading-spinner loading-lg"></span>
		</div>
	{:else if form.data}
		<div class="space-y-4">
			<TextField
				label="Plex URL"
				type="url"
				placeholder="http://plex-server:32400"
				bind:value={form.data.plex_url}
			/>

			<SecretField
				label="Plex Token"
				mask={TOKEN_MASK}
				placeholder="Paste your Plex token"
				help="Sign in with Plex below, or paste a token here."
				bind:value={form.data.plex_token}
			/>

			<div class="flex items-center gap-3">
				{#if oauthPending}
					<span class="loading loading-spinner loading-sm"></span>
					<span class="text-sm text-base-content/70">Finish signing in to Plex to continue.</span>
					{#if oauthUrl}
						<a href={oauthUrl} target="_blank" rel="noopener" class="link link-primary text-sm">
							Open sign-in page
						</a>
					{/if}
					<Button variant="ghost" size="xs" onclick={cancelOAuth}>Cancel</Button>
				{:else}
					<Button variant="primary" size="sm" onclick={startOAuth}>Sign in with Plex</Button>
				{/if}
			</div>

			{#if hasCredentials || libraries.length > 0 || loadingLibraries}
				<fieldset class="fieldset">
					<legend class="fieldset-legend flex w-full items-center justify-between">
						Music libraries
						<Button variant="ghost" size="xs" onclick={fetchLibraries} disabled={loadingLibraries}>
							Refresh
						</Button>
					</legend>
					{#if loadingLibraries}
						<span class="loading loading-spinner loading-sm"></span>
					{:else if libraries.length > 0}
						<div class="space-y-2">
							{#each libraries as lib (lib.key)}
								<label class="flex items-center gap-3 cursor-pointer">
									<input
										type="checkbox"
										class="checkbox checkbox-sm"
										checked={form.data.music_library_ids?.includes(lib.key) ?? false}
										onchange={() => toggleLibrary(lib.key)}
									/>
									<span>{lib.title}</span>
								</label>
							{/each}
						</div>
					{:else}
						<p class="text-sm text-base-content/50">
							Run a connection test to load your Plex libraries.
						</p>
					{/if}
					<p class="label whitespace-normal text-base-content/60">
						Choose the Plex libraries that contain your music.
					</p>
				</fieldset>
			{/if}

			<ToggleRow
				label="Scrobble to Plex"
				description="Send your listening history back to Plex."
				bind:checked={form.data.scrobble_to_plex}
			/>

			{#if form.testResult}
				<div
					class="alert"
					class:alert-success={form.testResult.valid}
					class:alert-error={!form.testResult.valid}
				>
					<span>{form.testResult.message}</span>
				</div>
			{/if}

			<ToggleRow
				label="Enable Plex integration"
				description={!hasLibrarySelected
					? 'Select at least one music library.'
					: !form.testResult?.valid && !form.wasAlreadyEnabled
						? 'Test the connection before turning Plex on.'
						: 'Browse and play your Plex music here.'}
				bind:checked={form.data.enabled}
				disabled={(!form.testResult?.valid && !form.wasAlreadyEnabled) || !hasLibrarySelected}
			/>

			<ToggleRow
				label="Allow login with Plex"
				description={!form.testResult?.valid && !form.wasAlreadySecondaryEnabled
					? 'Test and get a valid connection to enable'
					: 'Let users sign in to DroppedNeedle with their Plex account'}
				bind:checked={form.data.login_enabled}
				disabled={!form.testResult?.valid && !form.wasAlreadySecondaryEnabled}
			/>

			{#if form.message}
				<div
					class="alert"
					class:alert-success={form.messageType === 'success'}
					class:alert-error={form.messageType === 'error'}
				>
					<span>{form.message}</span>
				</div>
			{/if}

			<div class="flex justify-end gap-2 pt-2">
				<Button
					variant="ghost"
					loading={form.testing}
					disabled={!form.data.plex_url || !form.data.plex_token}
					onclick={test}
				>
					Test connection
				</Button>
				<Button variant="primary" loading={form.saving} onclick={save}>Save settings</Button>
			</div>
		</div>
	{/if}
</Card>
