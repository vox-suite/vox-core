ALTER TABLE public.space_nodes
    ADD COLUMN replaces uuid REFERENCES public.space_nodes(id) ON DELETE SET NULL,
    ADD COLUMN is_current boolean NOT NULL DEFAULT true,
    ADD COLUMN is_final boolean NOT NULL DEFAULT false;
CREATE INDEX space_nodes_replaces_idx ON public.space_nodes(replaces) WHERE replaces IS NOT NULL;
