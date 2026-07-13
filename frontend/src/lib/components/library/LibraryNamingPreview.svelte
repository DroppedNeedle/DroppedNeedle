<script lang="ts">
	// Client-side preview of the naming template on a fixed sample tag. The server
	// (NamingTemplateEngine) is the source of truth; this is UX only.
	interface Props {
		template: string;
	}

	let { template }: Props = $props();

	const SAMPLE: Record<string, string | number> = {
		artist: 'Radiohead',
		album: 'OK Computer',
		albumartist: 'Radiohead',
		initial: 'R',
		year: 1997,
		track: 2,
		title: 'Paranoid Android',
		ext: 'flac',
		disc: 1,
		genre: 'Alternative',
		medium: 'CD',
		musicbrainz_id: 'b1392450',
		artist_mbid: 'a74b1b7f'
	};

	function render(tpl: string): string {
		return tpl.replace(/\{(\w+)(?::0(\d+)d)?\}/g, (_m, name, pad) => {
			const v = SAMPLE[name];
			if (v === undefined) return '';
			return pad ? String(v).padStart(Number(pad), '0') : String(v);
		});
	}

	const preview = $derived(template ? render(template) : '');
</script>

<div class="mt-1 break-all rounded-box bg-base-200 p-2 font-mono text-xs text-base-content/70">
	{preview || '—'}
</div>
