<!-- A nested "advanced" disclosure inside a management profile editor section. -->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { ChevronRight } from 'lucide-svelte';

	interface Props {
		title: string;
		subtitle: string;
		/** Layout only, e.g. a grid column span. */
		class?: string;
		children: Snippet;
	}

	let { title, subtitle, class: className = '', children }: Props = $props();
</script>

<details class="management-editor-advanced {className}">
	<summary>
		<span><strong>{title}</strong><small>{subtitle}</small></span>
		<ChevronRight class="h-4 w-4" />
	</summary>
	{@render children()}
</details>
