<script lang="ts">
	import '../../auth.css';
	import { useQueryClient } from '@tanstack/svelte-query';
	import { ApiError, TransportError } from '$lib/api/errors';
	import {
		createUpgradeContinueMutation,
		getUpgradeProgressQuery,
		getUpgradeStateQuery,
		markUpgradeSignedOut
	} from '$lib/features/upgrade/queries/UpgradeQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import { ArrowUp, CloudOff } from 'lucide-svelte';
	import AuthCard from '$lib/ui/AuthCard.svelte';
	import AuthFrame from '$lib/ui/AuthFrame.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import BackOnV2 from '$lib/features/upgrade/components/BackOnV2.svelte';
	import Done from '$lib/features/upgrade/components/Done.svelte';
	import Failed from '$lib/features/upgrade/components/Failed.svelte';
	import Interrupted from '$lib/features/upgrade/components/Interrupted.svelte';
	import Opening from '$lib/features/upgrade/components/Opening.svelte';
	import Preflight from '$lib/features/upgrade/components/Preflight.svelte';
	import Progress from '$lib/features/upgrade/components/Progress.svelte';
	import SignIn from '$lib/features/upgrade/components/SignIn.svelte';

	const upgrade = getUpgradeStateQuery();
	const signedIn = $derived(upgrade.data?.signed_in === true);
	const progress = getUpgradeProgressQuery(() => signedIn);
	const continueApp = createUpgradeContinueMutation();
	const client = useQueryClient();

	// A run's ending stays on screen until "Try again" or "Upgrade after all"
	// sets it aside; the next run shows again.
	let setAside = $state.raw<object | null>(null);
	let opening = $state(false);
	let continueError = $state<string | null>(null);

	const run = $derived(progress.data?.run ?? null);
	const ended = $derived(run && !run.running && run !== setAside ? run : null);
	const layout = $derived(progress.data?.layout ?? upgrade.data?.layout);

	// The server forgot this sign-in (it lasts 12 hours, and ends with the
	// mode): sign in again.
	$effect(() => {
		if (progress.error instanceof ApiError && progress.error.status === 401) {
			void markUpgradeSignedOut(client);
		}
	});

	const TASK_NAMES = { upgrade: 'The upgrade', fresh: 'The fresh start', undo: 'Going back' };

	async function openApp() {
		continueError = null;
		try {
			await continueApp.mutateAsync();
			opening = true;
		} catch (cause) {
			// The upgrade page stops as it answers, so the answer can be lost:
			// wait for the app either way, unless the server said no.
			if (cause instanceof TransportError) opening = true;
			else continueError = cause instanceof ApiError ? cause.message : 'Could not reach the server';
		}
	}

	function notUpgradeMode(cause: unknown): boolean {
		return cause instanceof ApiError && cause.status === 404;
	}
</script>

<svelte:head>
	<title>Upgrade - DroppedNeedle</title>
</svelte:head>

<AuthFrame kicker="Upgrade from v2" width="lg">
	<QueryBoundary query={upgrade} what="the upgrade">
		{#snippet failed(cause, retry)}
			{#if notUpgradeMode(cause)}
				<AuthCard icon={ArrowUp} title="Nothing to upgrade">
					<p class="text-sm text-muted">This server already runs v3.</p>
					{#snippet actions()}
						<Button variant="primary" size="display" href={withBasePath('/')}
							>Open DroppedNeedle</Button
						>
					{/snippet}
				</AuthCard>
			{:else}
				<AuthCard icon={CloudOff} title="Can't reach the server">
					<p class="text-sm text-muted">Check that it is running, then try again.</p>
					{#snippet actions()}
						<Button onclick={retry}>Try again</Button>
					{/snippet}
				</AuthCard>
			{/if}
		{/snippet}
		{#snippet children(current)}
			{#if !current.signed_in}
				<SignIn />
			{:else if opening}
				<Opening />
			{:else if !progress.data}
				{#if progress.isError}
					<AuthCard icon={CloudOff} title="Can't reach the server">
						<p class="text-sm text-muted">Check that it is running, then try again.</p>
						{#snippet actions()}
							<Button onclick={() => void progress.refetch()}>Try again</Button>
						{/snippet}
					</AuthCard>
				{:else}
					<AuthCard>
						<span class="loading loading-spinner loading-sm" aria-label="Loading"></span>
					</AuthCard>
				{/if}
			{:else if run?.running}
				<Progress {run} disconnected={progress.isError} />
			{:else if ended?.failure}
				<Failed
					failure={ended.failure}
					what={TASK_NAMES[ended.task]}
					onretry={() => (setAside = ended)}
				/>
			{:else if ended?.result?.outcome === 'undone'}
				<BackOnV2 onupgrade={() => (setAside = ended)} />
			{:else if ended?.result || layout === 'finished'}
				<Done
					result={ended?.result ?? run?.result ?? null}
					continuing={continueApp.isPending}
					{continueError}
					oncontinue={() => void openApp()}
				/>
			{:else if layout === 'interrupted'}
				<Interrupted />
			{:else}
				<Preflight />
			{/if}
		{/snippet}
	</QueryBoundary>
</AuthFrame>
