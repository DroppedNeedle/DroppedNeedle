<script lang="ts">
	import { CircleAlert, Clock, Settings, WifiOff } from 'lucide-svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { describeServiceError } from '$lib/utils/serviceError';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		error: unknown;
		/** What failed to load, for the sentence: "Home", "Discover". */
		what: string;
		onretry?: () => void;
	}

	let { error, what, onretry }: Props = $props();
	const view = $derived(describeServiceError(error, what));
</script>

<div class="mt-16 flex flex-col items-center justify-center px-4 text-center" role="status">
	{#if view.kind === 'rate_limited'}
		<Clock class="mb-4 h-10 w-10 text-warning" />
	{:else if view.kind === 'not_configured'}
		<Settings class="mb-4 h-10 w-10 text-base-content/50" />
	{:else if view.kind === 'offline'}
		<WifiOff class="mb-4 h-10 w-10 text-base-content/50" />
	{:else}
		<CircleAlert class="mb-4 h-10 w-10 text-base-content/50" />
	{/if}
	<h2 class="text-lg font-semibold">{view.title}</h2>
	<p class="mt-1 max-w-md text-base-content/70">{view.message}</p>
	<div class="mt-4 flex gap-2">
		{#if view.settingsHref && authStore.isAdmin}
			<Button variant="primary" href={withBasePath(view.settingsHref)}>Open settings</Button>
		{/if}
		{#if view.canRetry && onretry}
			<Button variant={view.settingsHref ? 'ghost' : 'primary'} onclick={onretry}>Try again</Button>
		{/if}
	</div>
	{#if view.settingsHref && !authStore.isAdmin}
		<p class="mt-3 text-sm text-base-content/55">
			Ask the person who runs this server to set it up.
		</p>
	{/if}
</div>
