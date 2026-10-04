<script lang="ts">
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import { api } from '$lib/api/client';
	import { PLEX_ENDPOINTS } from '$lib/queries/plex/endpoints';
	import { pollPlexFlow, startPlexFlow } from '$lib/queries/plex/PlexFlowApi';
	import { resetPlexScrobblePreference } from '$lib/player/plexPlaybackApi';
	import type { components } from '$lib/api/v3/openapi';

	type PlexConnectionSettings = components['schemas']['PlexConnectionDto'];
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

	let showToken = $state(false);
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

<div class="card bg-base-200">
	<div class="card-body">
		<h2 class="card-title text-2xl">Plex Connection</h2>
		<p class="text-base-content/70 mb-4">
			Connect Plex to browse your library, play music, and keep your listening history in sync.
		</p>

		{#if form.loading}
			<div class="flex justify-center items-center py-12">
				<span class="loading loading-spinner loading-lg"></span>
			</div>
		{:else if form.data}
			<div class="space-y-4">
				<div class="form-control w-full">
					<label class="label" for="plex-url">
						<span class="label-text">Plex URL</span>
					</label>
					<input
						id="plex-url"
						type="url"
						bind:value={form.data.plex_url}
						class="input input-bordered w-full"
						placeholder="http://plex-server:32400"
					/>
				</div>

				<div class="form-control w-full">
					<label class="label" for="plex-token">
						<span class="label-text">Plex Token</span>
					</label>
					<div class="join w-full">
						<input
							id="plex-token"
							type={showToken ? 'text' : 'password'}
							bind:value={form.data.plex_token}
							class="input input-bordered join-item flex-1"
							placeholder="Paste your Plex token"
						/>
						<button type="button" class="btn join-item" onclick={() => (showToken = !showToken)}>
							{showToken ? 'Hide' : 'Show'}
						</button>
					</div>
					<div class="label">
						<span class="label-text-alt text-base-content/50">
							Sign in with Plex below, or paste a token here.
						</span>
					</div>
				</div>

				<div class="flex items-center gap-3">
					{#if oauthPending}
						<span class="loading loading-spinner loading-sm"></span>
						<span class="text-sm text-base-content/70">Finish signing in to Plex to continue.</span>
						{#if oauthUrl}
							<a href={oauthUrl} target="_blank" rel="noopener" class="link link-primary text-sm">
								Open sign-in page
							</a>
						{/if}
						<button type="button" class="btn btn-ghost btn-xs" onclick={cancelOAuth}>
							Cancel
						</button>
					{:else}
						<button
							type="button"
							class="btn btn-sm"
							style="background-color: rgb(var(--brand-plex)); color: #000;"
							onclick={startOAuth}
						>
							Sign in with Plex
						</button>
					{/if}
				</div>

				{#if hasCredentials || libraries.length > 0 || loadingLibraries}
					<div class="form-control w-full">
						<label class="label">
							<span class="label-text">Music libraries</span>
							<button
								type="button"
								class="btn btn-xs btn-ghost"
								onclick={fetchLibraries}
								disabled={loadingLibraries}
							>
								Refresh
							</button>
						</label>
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
										<span class="label-text">{lib.title}</span>
									</label>
								{/each}
							</div>
						{:else}
							<p class="text-sm text-base-content/50">
								Run a connection test to load your Plex libraries.
							</p>
						{/if}
						<div class="label">
							<span class="label-text-alt text-base-content/50">
								Choose the Plex libraries that contain your music.
							</span>
						</div>
					</div>
				{/if}

				<div class="form-control">
					<label class="label cursor-pointer justify-start gap-4">
						<input
							type="checkbox"
							bind:checked={form.data.scrobble_to_plex}
							class="toggle toggle-sm"
						/>
						<div>
							<span class="label-text font-medium">Scrobble to Plex</span>
							<p class="text-xs text-base-content/50">Send your listening history back to Plex.</p>
						</div>
					</label>
				</div>

				{#if form.testResult}
					<div
						class="alert"
						class:alert-success={form.testResult.valid}
						class:alert-error={!form.testResult.valid}
					>
						<span>{form.testResult.message}</span>
					</div>
				{/if}

				<div class="form-control">
					<label class="label cursor-pointer justify-start gap-4">
						<input
							type="checkbox"
							bind:checked={form.data.enabled}
							class="toggle toggle-primary"
							disabled={(!form.testResult?.valid && !form.wasAlreadyEnabled) || !hasLibrarySelected}
						/>
						<div>
							<span class="label-text font-medium">Enable Plex integration</span>
							<p class="text-xs text-base-content/50">
								{#if !hasLibrarySelected}
									Select at least one music library.
								{:else if !form.testResult?.valid && !form.wasAlreadyEnabled}
									Test the connection before turning Plex on.
								{:else}
									Browse and play your Plex music here.
								{/if}
							</p>
						</div>
					</label>
				</div>

				<div class="form-control">
					<label class="label cursor-pointer justify-start gap-4">
						<input
							type="checkbox"
							bind:checked={form.data.login_enabled}
							class="toggle toggle-primary"
							disabled={!form.testResult?.valid && !form.wasAlreadySecondaryEnabled}
						/>
						<div>
							<span class="label-text font-medium">Allow login with Plex</span>
							<p class="text-xs text-base-content/50">
								{#if !form.testResult?.valid && !form.wasAlreadySecondaryEnabled}
									Test and get a valid connection to enable
								{:else}
									Let users sign in to DroppedNeedle with their Plex account
								{/if}
							</p>
						</div>
					</label>
				</div>

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
					<button
						type="button"
						class="btn btn-ghost"
						onclick={test}
						disabled={form.testing || !form.data.plex_url || !form.data.plex_token}
					>
						{#if form.testing}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Test connection
					</button>
					<button type="button" class="btn btn-primary" onclick={save} disabled={form.saving}>
						{#if form.saving}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Save settings
					</button>
				</div>
			</div>
		{/if}
	</div>
</div>
