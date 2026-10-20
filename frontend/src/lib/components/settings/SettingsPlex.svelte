<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { onDestroy } from 'svelte';
	import { api } from '$lib/api/client';
	import { PLEX_ENDPOINTS } from '$lib/queries/plex/endpoints';
	import { pollPlexFlow, startPlexFlow } from '$lib/queries/plex/PlexFlowApi';
	import {
		createSavePlexSettingsMutation,
		createVerifyPlexMutation,
		getPlexSettingsQuery,
		type PlexSettings
	} from '$lib/queries/settings/SectionSettingsQueries.svelte';
	import type { components } from '$lib/api/v3/openapi';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	type PlexLibrarySection = components['schemas']['PlexLibrarySectionInfo'];

	let oauthPending = $state(false);
	let oauthUrl = $state<string | null>(null);
	let libraries = $state<PlexLibrarySection[]>([]);
	let loadingLibraries = $state(false);

	const verify = createVerifyPlexMutation();

	const section = createSettingsSection(getPlexSettingsQuery(), createSavePlexSettingsMutation(), {
		refreshIntegration: true,
		test: async (draft) => {
			if (!draft.plex_url || !draft.plex_token) {
				return { passed: false, message: 'Add the URL and token first.' };
			}
			const result = await verify.mutateAsync(draft);
			if (result.valid) {
				libraries = result.libraries ?? [];
				const found = new Set(libraries.map((library) => library.key));
				draft.music_library_ids = (draft.music_library_ids ?? []).filter((id) => found.has(id));
			}
			return { passed: result.valid, message: result.message };
		}
	});

	// The switches stay locked until a test passes, unless already on.
	const enableLocked = $derived(!section.testPassed && !section.saved?.enabled);
	const loginLocked = $derived(!section.testPassed && !section.saved?.login_enabled);

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
					if (section.draft) section.draft.plex_token = res.auth_token;
					oauthPending = false;
					oauthUrl = null;
					await section.test();
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

	// A saved connection lists its libraries once, when the settings arrive.
	let librariesAsked = false;
	$effect(() => {
		const saved = section.saved;
		if (!saved || librariesAsked) return;
		librariesAsked = true;
		if (saved.plex_url && saved.plex_token) void fetchLibraries();
	});

	function toggleLibrary(draft: PlexSettings, key: string) {
		const ids = draft.music_library_ids ?? [];
		draft.music_library_ids = ids.includes(key) ? ids.filter((k) => k !== key) : [...ids, key];
	}

	onDestroy(() => {
		oauthPending = false;
	});
</script>

<Card title="Plex Connection">
	<p class="text-base-content/70 mb-4">
		Connect Plex to browse your library, play music, and keep your listening history in sync.
	</p>

	<QueryBoundary query={section} what="the Plex settings">
		{#snippet children(draft)}
			<div class="space-y-4">
				<TextField
					label="Plex URL"
					type="url"
					placeholder="http://plex-server:32400"
					bind:value={draft.plex_url}
				/>

				<SecretField
					label="Plex Token"
					mask={SECRET_MASK}
					placeholder="Paste your Plex token"
					help="Sign in with Plex below, or paste a token here."
					bind:value={draft.plex_token}
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

				{#if (draft.plex_url && draft.plex_token) || libraries.length > 0 || loadingLibraries}
					<fieldset class="fieldset">
						<legend class="fieldset-legend flex w-full items-center justify-between">
							Music libraries
							<Button
								variant="ghost"
								size="xs"
								onclick={fetchLibraries}
								disabled={loadingLibraries}
							>
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
											checked={draft.music_library_ids?.includes(lib.key) ?? false}
											onchange={() => toggleLibrary(draft, lib.key)}
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
					label="Enable Plex integration"
					description={!draft.music_library_ids?.length
						? 'Select at least one music library.'
						: enableLocked
							? 'Test the connection first.'
							: 'Browse and play your Plex music here.'}
					bind:checked={draft.enabled}
					disabled={enableLocked || !draft.music_library_ids?.length}
				/>

				<ToggleRow
					label="Allow login with Plex"
					description={loginLocked
						? 'Test the connection first.'
						: 'Let users sign in to DroppedNeedle with their Plex account'}
					bind:checked={draft.login_enabled}
					disabled={loginLocked}
				/>

				<SaveBar {...section.bar} testLabel="Test connection" />
			</div>
		{/snippet}
	</QueryBoundary>
</Card>
