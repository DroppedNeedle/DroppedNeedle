<script lang="ts">
	import { LogOut, Monitor, Smartphone, Trash2, TriangleAlert } from 'lucide-svelte';
	import { getSessionsQuery } from '$lib/features/profile/queries/sessions.svelte';
	import {
		createRevokeSessionMutation,
		createSignOutEverywhereMutation
	} from '$lib/features/profile/queries/sessions.svelte';
	import type { SessionView } from '$lib/features/profile/queries/sessions.svelte';
	import Button from '$lib/ui/Button.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';

	const sessionsQuery = getSessionsQuery();
	// this browser's session first; a long list shows its first few until asked
	const sessions = $derived(
		[...(sessionsQuery.data?.sessions ?? [])].sort((a, b) => Number(b.current) - Number(a.current))
	);
	const FIRST_FEW = 5;
	let showAll = $state(false);
	const shown = $derived(showAll ? sessions : sessions.slice(0, FIRST_FEW));
	const revoke = createRevokeSessionMutation();
	const signOutEverywhere = createSignOutEverywhereMutation();

	let confirmingId = $state<string | null>(null);
	let signOutAllOpen = $state(false);

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

<p class="pf-group">
	{#if sessionsQuery.isPending}
		Loading signed-in devices
	{:else if sessions.length === 1}
		1 signed-in session
	{:else}
		{sessions.length.toLocaleString()} signed-in sessions
	{/if}
</p>

{#if sessionsQuery.isPending}
	<div data-testid="sessions-skeleton" class="flex flex-col gap-3">
		{#each [0, 1] as i (`session-skeleton-${i}`)}
			<div class="flex animate-pulse items-center gap-3">
				<div class="size-11 shrink-0 rounded-lg bg-base-300"></div>
				<div class="flex-1">
					<div class="mb-1.5 h-4 w-40 rounded bg-base-300"></div>
					<div class="h-3 w-28 rounded bg-base-300"></div>
				</div>
			</div>
		{/each}
	</div>
{:else if sessionsQuery.isError}
	<div class="alert alert-warning">
		<TriangleAlert class="h-5 w-5" />
		<span>Could not load sessions. Check the connection and try again.</span>
		<Button size="sm" onclick={() => sessionsQuery.refetch()}>Retry</Button>
	</div>
{:else if sessions.length === 0}
	<p class="text-sm text-muted">No active sessions found.</p>
{:else}
	<ul class="pf-rows">
		{#each shown as session (session.id)}
			<li class="pf-row ss-row">
				<span class="pf-tile" aria-hidden="true">
					{#if session.kind === 'companion'}
						<Smartphone class="size-5" />
					{:else}
						<Monitor class="size-5" />
					{/if}
				</span>
				<div class="pf-row__main">
					<p class="truncate text-sm font-semibold" title={session.label}>{session.label}</p>
					<p class="pf-meta mt-0.5">
						Last seen {relativeTime(session.last_seen_at)} · Signed in {formatDate(
							session.created_at
						)} · Expires {formatDate(session.expires_at)}
					</p>
				</div>
				<div class="ss-row__end">
					<StatusChip size="xs">{kindLabel(session.kind)}</StatusChip>
					{#if session.current}
						<StatusChip tone="action" size="xs">This session</StatusChip>
					{:else if confirmingId === session.id}
						<Button
							variant="ghost"
							size="xs"
							onclick={() => (confirmingId = null)}
							disabled={revoke.isPending}
						>
							Keep session
						</Button>
						<Button
							variant="danger"
							size="xs"
							loading={revoke.isPending}
							onclick={() => void confirmRevoke(session)}
						>
							Confirm revoke
						</Button>
					{:else}
						<Button
							variant="outline"
							size="sm"
							icon={Trash2}
							aria-label="Revoke {session.label}"
							onclick={() => (confirmingId = session.id)}
						>
							Revoke
						</Button>
					{/if}
				</div>
			</li>
		{/each}
	</ul>
	{#if sessions.length > FIRST_FEW}
		<Button variant="caps" size="sm" class="mt-1" onclick={() => (showAll = !showAll)}>
			{showAll ? 'Show fewer' : `Show all ${sessions.length.toLocaleString()} sessions`}
		</Button>
	{/if}
{/if}
<div class="ss-foot">
	<Button
		variant="danger-outline"
		size="display-sm"
		icon={LogOut}
		onclick={() => (signOutAllOpen = true)}
	>
		Sign out everywhere
	</Button>
</div>

<ConfirmDialog
	bind:open={signOutAllOpen}
	title="Sign out everywhere?"
	message="Every browser and device is signed out, this one too. App passwords keep working."
	confirmLabel="Sign out everywhere"
	tone="danger"
	onconfirm={() => signOutEverywhere.mutateAsync()}
/>

<style>
	.ss-row__end {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: flex-end;
		gap: 0.5rem 0.75rem;
	}
	.ss-foot {
		margin-top: 0.75rem;
		padding-top: 0.875rem;
		border-top: 1px solid var(--dn-line);
	}
	@media (max-width: 639px) {
		.ss-row__end {
			grid-column: 2;
			justify-content: flex-start;
		}
	}
</style>
