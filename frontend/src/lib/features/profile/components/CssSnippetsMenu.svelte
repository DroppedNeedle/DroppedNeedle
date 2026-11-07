<!-- Starter rules, each handed to `oninsert` to go in at the cursor. -->
<script lang="ts">
	import { Plus } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import { CSS_SNIPPETS } from './cssVocabulary';

	interface Props {
		oninsert: (text: string) => void;
		disabled?: boolean;
	}

	let { oninsert, disabled = false }: Props = $props();
</script>

<ul class="css-rail-list" aria-label="Snippets">
	{#each CSS_SNIPPETS as snippet (snippet.id)}
		<li>
			<Button
				variant="ghost"
				size="sm"
				icon={Plus}
				class="w-full justify-start"
				{disabled}
				title="Insert at the cursor"
				onclick={() => oninsert(`\n${snippet.css}`)}
			>
				{snippet.label}
			</Button>
		</li>
	{/each}
</ul>
