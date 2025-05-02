#ifndef TASK_INFO_H
#define TASK_INFO_H

#define TASK_COMM_LEN 16
#define CGROUP_NAME_LEN 64

struct task_info {
	u32 pid;
	u32 tgid;
	int cglevel;
	u8 name[TASK_COMM_LEN];
	u8 cg_name[CGROUP_NAME_LEN];
	u8 cg_pname[CGROUP_NAME_LEN];
	u8 cg_gpname[CGROUP_NAME_LEN];
};

#endif //TASK_INFO_H