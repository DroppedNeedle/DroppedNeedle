<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import type { RemoteSourceDescriptor } from '$lib/queries/remotes/remoteSources';
	import { withBasePath } from '$lib/utils/basePath';
	import { ChevronLeft } from 'lucide-svelte';
	import BrandLine from './BrandLine.svelte';

	interface Props {
		remote: RemoteSourceDescriptor;
		/** What the page lists: "Artists", "Tracks". */
		kind: string;
		total: number;
	}

	let { remote, kind, total }: Props = $props();
</script>

<BrandLine {remote} class="mb-6" />

<Card variant="bordered" padding="md" class="mb-6">
	<div class="flex items-center gap-3">
		<Button
			variant="ghost"
			size="sm"
			icon={ChevronLeft}
			href={withBasePath(`/library/${remote.source}`)}
			aria-label="Back to {remote.label} library"
		>
			Back
		</Button>
		<span class={remote.tone}>
			<remote.icon class="h-6 w-6" />
		</span>
		<h1 class="text-2xl font-bold">{remote.label} {kind}</h1>
		{#if total > 0}
			<span class="badge badge-ghost">{total}</span>
		{/if}
	</div>
</Card>
