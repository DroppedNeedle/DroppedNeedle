<script lang="ts">
	import { CircleAlert } from 'lucide-svelte';
	import type { UpgradeFailure } from '$lib/features/upgrade/queries/UpgradeQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { failureFix } from './upgradeText';

	interface Props {
		failure: UpgradeFailure;
		/** What failed: the upgrade, the fresh start or going back. */
		what: string;
		onretry: () => void;
	}

	let { failure, what, onretry }: Props = $props();

	const V2_FILES = {
		untouched: 'Your v2 files were not moved.',
		put_back: 'Your v2 files are back where they were.',
		in_backup: 'Your v2 files are in the v2-backup folders. Go back to v2 puts them back.'
	};
</script>

<Card variant="bordered" title="{what} failed">
	<div class="flex flex-col gap-3 text-sm">
		<div class="alert alert-error items-start" role="alert">
			<CircleAlert class="size-4 shrink-0" />
			<div>
				<p>{failure.message}</p>
				<p class="mt-1 font-medium">{failureFix(failure.code)}</p>
			</div>
		</div>
		<p class="text-base-content/70">{V2_FILES[failure.v2_files]}</p>
	</div>
	{#snippet actions()}
		<Button variant="primary" onclick={onretry}>Try again</Button>
	{/snippet}
</Card>
