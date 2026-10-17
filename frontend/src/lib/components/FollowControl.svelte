<script lang="ts">
	import { Heart, Clock } from 'lucide-svelte';
	import { getFollowStatusQuery } from '$lib/queries/following/FollowQueries.svelte';
	import {
		createSetAutoDownloadMutation,
		createSetFollowMutation
	} from '$lib/queries/following/FollowMutations.svelte';
	import type { FollowStatus } from '$lib/queries/following/types';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		artistMbid: string;
	}

	let { artistMbid }: Props = $props();

	const NOT_FOLLOWING: FollowStatus = {
		followed: false,
		auto_download: false,
		auto_download_state: 'none'
	};

	const statusQuery = getFollowStatusQuery(() => artistMbid);
	const followMutation = createSetFollowMutation(() => artistMbid);
	const autoDownloadMutation = createSetAutoDownloadMutation(() => artistMbid);

	const status = $derived(statusQuery.data ?? NOT_FOLLOWING);
	const busy = $derived(
		statusQuery.isPending || followMutation.isPending || autoDownloadMutation.isPending
	);
	const declined = $derived(
		status.auto_download_state === 'rejected' || status.auto_download_state === 'revoked'
	);
</script>

<div class="flex flex-col gap-2">
	<Button
		variant={status.followed ? 'accent' : 'outline'}
		size="sm"
		class="w-fit"
		onclick={() => followMutation.mutate(!status.followed)}
		disabled={busy}
		aria-pressed={status.followed}
		aria-label={status.followed ? 'Following this artist, click to unfollow' : 'Follow this artist'}
	>
		<Heart class="h-4 w-4" fill={status.followed ? 'currentColor' : 'none'} aria-hidden="true" />
		{status.followed ? 'Following' : 'Follow'}
	</Button>

	{#if status.followed}
		<div class="flex flex-wrap items-center gap-2 pl-2">
			<label class="label cursor-pointer gap-2 py-0">
				<input
					type="checkbox"
					checked={status.auto_download}
					onchange={() => autoDownloadMutation.mutate(!status.auto_download)}
					disabled={busy}
					class="toggle toggle-sm toggle-primary"
				/>
				<span class="text-sm text-base-content/70">Auto-download new releases</span>
			</label>

			{#if status.auto_download && status.auto_download_state === 'pending'}
				<span class="badge badge-warning gap-1" aria-label="Pending admin approval">
					<Clock class="h-3 w-3" />
					Pending admin approval
				</span>
			{:else if declined}
				<span class="text-xs text-base-content/50">Approval declined</span>
			{/if}
		</div>
	{/if}
</div>
