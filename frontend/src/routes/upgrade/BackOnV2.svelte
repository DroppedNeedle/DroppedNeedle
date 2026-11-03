<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';

	interface Props {
		onupgrade: () => void;
	}

	let { onupgrade }: Props = $props();
</script>

<Card variant="bordered" title="Back on v2">
	<p class="text-sm text-base-content/70">Your v2 files are back where they were. Now:</p>
	<ol class="mt-2 list-decimal space-y-1 pl-5 text-sm">
		<li>Stop the server.</li>
		<li>Set the image tag in your compose file back to your v2 version.</li>
		<li>Start it again.</li>
	</ol>
	{#snippet actions()}
		<Button variant="ghost" size="sm" onclick={onupgrade}>Upgrade after all</Button>
	{/snippet}
</Card>
