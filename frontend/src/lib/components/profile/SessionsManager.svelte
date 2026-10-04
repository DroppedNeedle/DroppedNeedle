<script lang="ts">
	import { KeyRound, Monitor, Smartphone, TriangleAlert } from 'lucide-svelte';
	import { getSessionsQuery } from '$lib/queries/sessions/SessionsQuery.svelte';
	import { createRevokeSessionMutation } from '$lib/queries/sessions/SessionsMutations.svelte';
	import type { SessionView } from '$lib/queries/sessions/types';

	const sessionsQuery = getSessionsQuery();
	const sessions = $derived(sessionsQuery.data?.sessions ?? []);
	const revoke = createRevokeSessionMutation();

	let confirmingId = $state<string | null>(null);

	function kindLabel(kind: string): string {
		if (kind === 'companion') return 'App';
		if (kind === 'standard') return 'Browser';
		return kind;
	}

	function relativeTime(epochSeconds: number): string {
		const diff = Date.now() / 1000 - epochSeconds;
		if (diff < 60) return 'just now';
		if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
		if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`;
		return `${Math.floor(diff / 86400)}d ago`;
	}

	function formatDate(epochSeconds: number): string {
		return new Date(epochSeconds * 1000).toLocaleDateString(undefined, {
			month: 'short',
			day: 'numeric',
			year: 'numeric'
		});
	}

	async function confirmRevoke(session: SessionView) {
		try {
			await revoke.mutateAsync({ id: session.id, label: session.label });
		} finally {
			confirmingId = null;
		}
	}
</script>

<div class="overflow-hidden rounded-xl border border-base-300/40 bg-base-200/50 backdrop-blur-sm">
	<div class="flex items-center gap-3 px-5 py-4">
		<div
			class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-base-300/60 text-base-content/70"
		>
			<KeyRound class="h-4 w-4" />
		</div>
		<div class="min-w-0 flex-1">
			<h2 class="text-sm font-semibold">Sessions</h2>
			<p class="text-xs text-base-content/50">
				{#if sessionsQuery.isPending}
					Loading signed-in devices
				{:else if sessions.length === 1}
					1 signed-in session
				{:else}
					{sessions.length} signed-in sessions
				{/if}
			</p>
		</div>
	</div>

	{#if sessionsQuery.isPending}
		<div data-testid="sessions-skeleton" class="flex flex-col gap-3 px-5 pb-5">
			{#each [0, 1] as i (`session-skeleton-${i}`)}
				<div class="flex animate-pulse items-center gap-3">
					<div class="h-9 w-9 shrink-0 rounded-lg bg-base-300"></div>
					<div class="flex-1">
						<div class="mb-1.5 h-4 w-40 rounded bg-base-300"></div>
						<div class="h-3 w-28 rounded bg-base-300"></div>
					</div>
				</div>
			{/each}
		</div>
	{:else if sessionsQuery.isError}
		<div class="px-5 pb-5">
			<div class="alert alert-warning">
				<TriangleAlert class="h-5 w-5" />
				<span>Could not load sessions. Check the connection and try again.</span>
				<button class="btn btn-sm" onclick={() => sessionsQuery.refetch()}>Retry</button>
			</div>
		</div>
	{:else if sessions.length === 0}
		<p class="px-5 pb-5 text-sm text-base-content/50">No active sessions found.</p>
	{:else}
		<ul class="divide-y divide-base-300/30 border-t border-base-300/30">
			{#each sessions as session (session.id)}
				<li class="flex items-start gap-3 px-5 py-4">
					<div
						class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-base-300/60 text-base-content/70"
					>
						{#if session.kind === 'companion'}
							<Smartphone class="h-4 w-4" />
						{:else}
							<Monitor class="h-4 w-4" />
						{/if}
					</div>
					<div class="min-w-0 flex-1">
						<p class="truncate text-sm font-medium">{session.label}</p>
						<p class="mt-0.5 text-xs text-base-content/50">
							Last seen {relativeTime(session.last_seen_at)} · Signed in {formatDate(
								session.created_at
							)} · Expires {formatDate(session.expires_at)}
						</p>
						<div class="mt-1.5 flex flex-wrap items-center gap-1.5">
							<span class="badge badge-ghost badge-sm">{kindLabel(session.kind)}</span>
							{#if session.current}
								<span class="badge badge-primary badge-sm">This session</span>
							{/if}
						</div>
					</div>
					{#if !session.current}
						<div class="flex shrink-0 items-center gap-1.5">
							{#if confirmingId === session.id}
								<button
									class="btn btn-ghost btn-xs"
									onclick={() => (confirmingId = null)}
									disabled={revoke.isPending}
								>
									Keep session
								</button>
								<button
									class="btn btn-error btn-xs"
									onclick={() => void confirmRevoke(session)}
									disabled={revoke.isPending}
								>
									{#if revoke.isPending}
										<span class="loading loading-spinner loading-xs"></span>
									{/if}
									Confirm revoke
								</button>
							{:else}
								<button
									class="btn btn-ghost btn-sm text-error"
									aria-label="Revoke {session.label}"
									onclick={() => (confirmingId = session.id)}
								>
									Revoke
								</button>
							{/if}
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</div>
