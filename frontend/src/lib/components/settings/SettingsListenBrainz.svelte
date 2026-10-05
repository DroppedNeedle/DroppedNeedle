<script lang="ts">
	import { AudioLines, ExternalLink } from 'lucide-svelte';
	import {
		getListenBrainzConnectionQuery,
		getScrobbleTargetsQuery
	} from '$lib/queries/listenbrainz/ListenBrainzQuery.svelte';
	import {
		createSaveListenBrainzMutation,
		createSaveScrobbleTargetsMutation,
		createVerifyListenBrainzMutation
	} from '$lib/queries/listenbrainz/ListenBrainzMutations.svelte';
	import type {
		ListenBrainzConnection,
		ScrobbleSettings,
		VerifyConnectionResponse
	} from '$lib/queries/listenbrainz/types';

	const connQuery = getListenBrainzConnectionQuery();
	const targetsQuery = getScrobbleTargetsQuery();
	const saveConn = createSaveListenBrainzMutation();
	const saveTargets = createSaveScrobbleTargetsMutation();
	const verify = createVerifyListenBrainzMutation();

	let connDraft = $state<ListenBrainzConnection | null>(null);
	let connDirty = $state(false);
	let targetsDraft = $state<ScrobbleSettings | null>(null);
	let targetsDirty = $state(false);
	let showToken = $state(false);
	let verdict = $state<VerifyConnectionResponse | null>(null);

	interface FormMessage {
		type: 'success' | 'error';
		text: string;
	}

	let connMessage = $state<FormMessage | null>(null);
	let targetsMessage = $state<FormMessage | null>(null);

	// Sync server data into the editable drafts until the admin touches them,
	// so a background refetch never clobbers half-typed input.
	$effect(() => {
		const data = connQuery.data;
		if (data && !connDirty) connDraft = { ...data };
	});
	$effect(() => {
		const data = targetsQuery.data;
		if (data && !targetsDirty) targetsDraft = { ...data };
	});

	function errorText(err: unknown, fallback: string): string {
		return err instanceof Error && err.message ? err.message : fallback;
	}

	async function saveConnection() {
		if (!connDraft) return;
		connMessage = null;
		verdict = null;
		try {
			const saved = await saveConn.mutateAsync(connDraft);
			connDraft = { ...saved };
			connDirty = false;
			connMessage = { type: 'success', text: 'ListenBrainz settings saved.' };
		} catch (err) {
			connMessage = { type: 'error', text: errorText(err, 'Could not save settings.') };
		}
	}

	async function testConnection() {
		if (!connDraft) return;
		connMessage = null;
		try {
			verdict = await verify.mutateAsync(connDraft);
		} catch (err) {
			verdict = { valid: false, message: errorText(err, 'Could not reach ListenBrainz.') };
		}
	}

	async function saveScrobbleTargets() {
		if (!targetsDraft) return;
		targetsMessage = null;
		try {
			const saved = await saveTargets.mutateAsync(targetsDraft);
			targetsDraft = { ...saved };
			targetsDirty = false;
			targetsMessage = { type: 'success', text: 'Scrobble targets saved.' };
		} catch (err) {
			targetsMessage = { type: 'error', text: errorText(err, 'Could not save targets.') };
		}
	}
</script>

<div class="flex flex-col gap-6">
	<div class="card border border-base-300/50 bg-base-200/60 backdrop-blur-sm">
		<div class="card-body gap-4">
			<div class="flex items-center gap-3">
				<div
					class="flex h-11 w-11 items-center justify-center rounded-xl bg-orange-500/10 text-orange-400 ring-1 ring-orange-500/20"
				>
					<AudioLines class="h-5 w-5" />
				</div>
				<div>
					<h2 class="card-title text-2xl">ListenBrainz</h2>
					<p class="text-sm text-base-content/60">Instance connection for scrobbling.</p>
				</div>
			</div>

			{#if connQuery.isPending}
				<div data-testid="listenbrainz-skeleton" class="flex animate-pulse flex-col gap-3 py-6">
					<div class="h-10 rounded-lg bg-base-300"></div>
					<div class="h-10 rounded-lg bg-base-300"></div>
					<div class="h-4 w-48 rounded bg-base-300"></div>
				</div>
			{:else if connQuery.isError || !connDraft}
				<div class="alert alert-warning">
					<span>Could not load ListenBrainz settings. Check the connection and try again.</span>
					<button class="btn btn-sm" onclick={() => connQuery.refetch()}>Retry</button>
				</div>
			{:else}
				<label class="flex cursor-pointer items-center gap-3">
					<input
						type="checkbox"
						class="toggle toggle-primary"
						bind:checked={connDraft.enabled}
						onchange={() => (connDirty = true)}
					/>
					<span class="text-sm font-medium">Enable ListenBrainz</span>
				</label>

				<label class="form-control w-full">
					<span
						class="label-text mb-1 text-xs font-semibold uppercase tracking-wider text-base-content/50"
					>
						Username
					</span>
					<input
						type="text"
						class="input input-soft w-full"
						bind:value={connDraft.username}
						oninput={() => (connDirty = true)}
						placeholder="Your ListenBrainz username"
						autocomplete="off"
					/>
				</label>

				<label class="form-control w-full">
					<span
						class="label-text mb-1 text-xs font-semibold uppercase tracking-wider text-base-content/50"
					>
						User token
					</span>
					<div class="relative">
						<input
							type={showToken ? 'text' : 'password'}
							class="input input-soft w-full pr-16"
							bind:value={connDraft.user_token}
							oninput={() => (connDirty = true)}
							placeholder="User token"
							autocomplete="off"
						/>
						<button
							type="button"
							class="btn btn-ghost btn-xs absolute right-2 top-1/2 -translate-y-1/2 rounded-full"
							onclick={() => (showToken = !showToken)}
						>
							{showToken ? 'Hide' : 'Show'}
						</button>
					</div>
				</label>
				<p class="-mt-2 text-xs text-base-content/50">
					Once saved, the token shows as a placeholder. Saving with the placeholder keeps the stored
					token.
				</p>

				<a
					href="https://listenbrainz.org/profile/"
					target="_blank"
					rel="noopener noreferrer"
					class="flex w-fit items-center gap-1 text-xs text-base-content/50 transition-colors hover:text-primary"
				>
					<ExternalLink class="h-3 w-3" /> Find your user token on your ListenBrainz profile
				</a>

				{#if verdict}
					<div
						role="status"
						class="alert"
						class:alert-success={verdict.valid}
						class:alert-error={!verdict.valid}
					>
						<span>{verdict.message}</span>
					</div>
				{/if}

				{#if connMessage}
					<div
						class="alert"
						class:alert-success={connMessage.type === 'success'}
						class:alert-error={connMessage.type === 'error'}
					>
						<span>{connMessage.text}</span>
					</div>
				{/if}

				<div class="flex justify-end gap-2 pt-1">
					<button
						type="button"
						class="btn btn-ghost gap-2 rounded-full"
						onclick={() => void testConnection()}
						disabled={verify.isPending || saveConn.isPending}
					>
						{#if verify.isPending}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Test connection
					</button>
					<button
						type="button"
						class="btn btn-primary glow-primary-soft gap-2 rounded-full"
						onclick={() => void saveConnection()}
						disabled={saveConn.isPending}
					>
						{#if saveConn.isPending}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Save ListenBrainz settings
					</button>
				</div>
			{/if}
		</div>
	</div>

	<div class="card border border-base-300/50 bg-base-200/60 backdrop-blur-sm">
		<div class="card-body gap-4">
			<div>
				<h2 class="card-title text-xl">Scrobble targets</h2>
				<p class="text-sm text-base-content/60">Where plays from this instance are reported.</p>
			</div>

			{#if targetsQuery.isPending || !targetsDraft}
				<div class="flex animate-pulse flex-col gap-3 py-2">
					<div class="h-6 w-56 rounded bg-base-300"></div>
					<div class="h-6 w-64 rounded bg-base-300"></div>
				</div>
			{:else}
				<label class="flex cursor-pointer items-center gap-3">
					<input
						type="checkbox"
						class="toggle toggle-primary"
						bind:checked={targetsDraft.scrobble_to_listenbrainz}
						onchange={() => (targetsDirty = true)}
					/>
					<span class="text-sm font-medium">Scrobble to ListenBrainz</span>
				</label>
				<label class="flex cursor-pointer items-center gap-3">
					<input
						type="checkbox"
						class="toggle toggle-primary"
						bind:checked={targetsDraft.scrobble_to_lastfm}
						onchange={() => (targetsDirty = true)}
					/>
					<span class="text-sm font-medium">Scrobble to Last.fm</span>
				</label>

				{#if targetsMessage}
					<div
						class="alert"
						class:alert-success={targetsMessage.type === 'success'}
						class:alert-error={targetsMessage.type === 'error'}
					>
						<span>{targetsMessage.text}</span>
					</div>
				{/if}

				<div class="flex justify-end pt-1">
					<button
						type="button"
						class="btn btn-primary glow-primary-soft gap-2 rounded-full"
						onclick={() => void saveScrobbleTargets()}
						disabled={saveTargets.isPending}
					>
						{#if saveTargets.isPending}
							<span class="loading loading-spinner loading-sm"></span>
						{/if}
						Save scrobble targets
					</button>
				</div>
			{/if}
		</div>
	</div>
</div>
