<script lang="ts">
	import JellyfinIcon from '$lib/components/JellyfinIcon.svelte';
	import PlexIcon from '$lib/components/PlexIcon.svelte';
	import { UserRound } from 'lucide-svelte';
	import { getImportCandidatesQuery } from '$lib/queries/auth/ImportCandidatesQuery.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { createImportUsersMutation } from '$lib/queries/auth/UserImportMutations.svelte';
	import { getApiUrl } from '$lib/api/api-utils';

	let { open = $bindable(false), onImported }: { open?: boolean; onImported?: () => void } =
		$props();

	let dialogEl: HTMLDialogElement | undefined = $state();
	let activeProvider = $state<'jellyfin' | 'plex'>('jellyfin');
	let selected = $state<string[]>([]);
	let broken = $state<string[]>([]);
	let resultMsg = $state<string | null>(null);
	let resultError = $state<string | null>(null);

	const candidatesQuery = getImportCandidatesQuery(
		() => activeProvider,
		() => open,
		() => authStore.user?.id
	);
	const candidates = $derived(candidatesQuery.data?.candidates ?? []);
	const importMutation = createImportUsersMutation();

	let wasOpen = false;
	$effect(() => {
		if (open && !wasOpen) {
			selected = [];
			resultMsg = null;
			resultError = null;
		}
		wasOpen = open;
		if (open) dialogEl?.showModal();
		else dialogEl?.close();
	});

	function switchProvider(provider: 'jellyfin' | 'plex') {
		if (provider === activeProvider) return;
		activeProvider = provider;
		selected = [];
		resultMsg = null;
		resultError = null;
	}

	function toggle(uid: string) {
		selected = selected.includes(uid) ? selected.filter((u) => u !== uid) : [...selected, uid];
	}

	async function runImport() {
		resultMsg = null;
		resultError = null;
		try {
			const res = await importMutation.mutateAsync({
				provider: activeProvider,
				provider_uids: selected
			});
			const parts = [`${res.total_imported} imported`];
			if (res.linked.length) parts.push(`${res.linked.length} linked to existing`);
			if (res.skipped.length) parts.push(`${res.skipped.length} skipped`);
			resultMsg = parts.join(', ');
			selected = [];
			onImported?.();
		} catch (e: unknown) {
			resultError = (e as { message?: string })?.message ?? 'Could not import users';
		}
	}

	function close() {
		open = false;
	}
</script>

<dialog bind:this={dialogEl} class="modal" onclose={close}>
	<div class="modal-box max-w-2xl">
		<h3 class="text-lg font-bold">Import Users</h3>
		<p class="text-sm text-base-content/60 mt-0.5">
			Pre-provision accounts from your media server. Each imported user signs in with their own
			Jellyfin or Plex login - no password is set here.
		</p>

		<div role="tablist" class="tabs tabs-boxed mt-4">
			<button
				role="tab"
				class="tab gap-2 {activeProvider === 'jellyfin' ? 'tab-active' : ''}"
				onclick={() => switchProvider('jellyfin')}
			>
				<JellyfinIcon class="h-4 w-4 text-info" />
				Jellyfin
			</button>
			<button
				role="tab"
				class="tab gap-2 {activeProvider === 'plex' ? 'tab-active' : ''}"
				onclick={() => switchProvider('plex')}
			>
				<PlexIcon class="h-4 w-4" style="color: rgb(var(--brand-plex))" />
				Plex
			</button>
		</div>

		<div class="mt-4 max-h-80 overflow-y-auto space-y-1.5">
			{#if candidatesQuery.isPending}
				{#each Array(3) as _, i (`import-skel-${i}`)}
					<div class="flex items-center gap-3 p-2.5 bg-base-300/40 rounded-box animate-pulse">
						<div class="w-5 h-5 rounded bg-base-300"></div>
						<div class="w-8 h-8 rounded-full bg-base-300"></div>
						<div class="flex-1">
							<div class="h-3.5 bg-base-300 rounded w-32"></div>
						</div>
					</div>
				{/each}
			{:else if candidatesQuery.isError}
				<div class="alert alert-error py-2 text-sm">Couldn't load accounts from this service.</div>
			{:else if candidates.length === 0}
				<div class="text-center py-10 text-sm text-base-content/50">
					No {activeProvider === 'plex' ? 'Plex' : 'Jellyfin'} accounts found. Check that this service
					is configured.
				</div>
			{:else}
				{#each candidates as candidate (candidate.provider_uid)}
					<label
						class="flex items-center gap-3 p-2.5 bg-base-300/30 rounded-box transition-colors {candidate.already_imported
							? 'opacity-50'
							: 'hover:bg-base-300/50 cursor-pointer'}"
					>
						<input
							type="checkbox"
							class="checkbox checkbox-sm"
							checked={selected.includes(candidate.provider_uid)}
							disabled={candidate.already_imported}
							onchange={() => toggle(candidate.provider_uid)}
						/>
						<div
							class="w-8 h-8 rounded-full bg-primary/10 flex items-center justify-center shrink-0 overflow-hidden"
						>
							{#if candidate.avatar_url && !broken.includes(candidate.provider_uid)}
								<img
									src={getApiUrl(candidate.avatar_url)}
									alt={candidate.display_name}
									class="h-full w-full object-cover"
									onerror={() => (broken = [...broken, candidate.provider_uid])}
								/>
							{:else}
								<UserRound class="h-4 w-4 text-primary/60" />
							{/if}
						</div>
						<div class="flex-1 min-w-0">
							<p class="text-sm font-medium truncate">{candidate.display_name}</p>
							{#if candidate.email}
								<p class="text-xs text-base-content/50 truncate">{candidate.email}</p>
							{/if}
						</div>
						{#if candidate.already_imported}
							<span class="badge badge-ghost badge-sm shrink-0">Already imported</span>
						{/if}
					</label>
				{/each}
			{/if}
		</div>

		{#if resultMsg}
			<div class="alert alert-success py-2 text-sm mt-3">{resultMsg}</div>
		{/if}
		{#if resultError}
			<div class="alert alert-error py-2 text-sm mt-3">{resultError}</div>
		{/if}

		<div class="modal-action">
			<button class="btn btn-ghost btn-sm" onclick={close}>Close</button>
			<button
				class="btn btn-primary btn-sm"
				disabled={selected.length === 0 || importMutation.isPending}
				onclick={() => void runImport()}
			>
				{#if importMutation.isPending}<span class="loading loading-spinner loading-xs"></span>{/if}
				Import{selected.length > 0 ? ` ${selected.length}` : ''}
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>
