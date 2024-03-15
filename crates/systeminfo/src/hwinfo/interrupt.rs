use super::util::*;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Interrupt {
    pub id: usize,
    pub cpu_affinity: Vec<usize>,
}

impl Interrupt {
    // TODO: parse the /proc/interrupts to add an interrupt name
    pub fn new(id: usize) -> Self {
        let cpu_affinity = read_list(format!("/proc/irq/{id}/smp_affinity_list")).unwrap();
        Interrupt { id, cpu_affinity }
    }
}

pub fn get_interrupts() -> Vec<Interrupt> {
    let mut ret = Vec::new();
    for irq in read_irqs("/proc/irq") {
        ret.push(Interrupt::new(irq));
    }
    ret
}
